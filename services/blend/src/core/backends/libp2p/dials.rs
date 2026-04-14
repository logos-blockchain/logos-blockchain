use core::{
    num::NonZeroU64,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use std::collections::{HashMap, hash_map::Entry};

use futures::{Stream, stream::FuturesUnordered};
use libp2p::{Multiaddr, PeerId};
use tracing::{debug, trace, warn};

use crate::core::backends::libp2p::LOG_TARGET;

#[derive(Clone, Debug)]
pub struct DialAttempt {
    /// Address of peer being dialed.
    pub address: Multiaddr,
    /// The latest (ongoing) attempt number.
    pub attempt_number: NonZeroU64,
}

/// [`DialAttempt`] with session information, i.e., whether the attempt was made
/// at this session or the previous one.
pub enum SessionDialAttempt {
    OngoingSession(Option<DialAttempt>),
    PreviousSession,
}

type PendingRetries = FuturesUnordered<Pin<Box<dyn Future<Output = PeerId> + Send>>>;

pub struct OngoingDials {
    dials: HashMap<PeerId, (DialAttempt, bool)>,
    retries: PendingRetries,
    max_attempts: NonZeroU64,
}

pub enum Error {
    CurrentlyRetrying,
}

impl OngoingDials {
    pub(super) fn new(max_attempts: NonZeroU64, capacity: usize) -> Self {
        Self {
            dials: HashMap::with_capacity(capacity),
            retries: FuturesUnordered::new(),
            max_attempts,
        }
    }

    /// Attempt to retry dialing the specified peer, if the maximum attempts
    /// have not already been performed.
    ///
    /// Returns:
    /// * `SessionDialAttempt::PreviousSession` if the peer is not being tracked
    ///   (a new session cleared the map);
    /// * `SessionDialAttempt::OngoingSession(None)` if a retry is scheduled
    ///   with exponential backoff;
    /// * `SessionDialAttempt::OngoingSession(Some(attempt))` if the maximum
    ///   attempts have been reached and the peer has been removed.
    ///
    /// Retries use exponential backoff: attempt 2 waits 2s, attempt 3 waits
    /// 4s, attempt N waits 2^(N-1) seconds.
    pub(super) fn schedule_retry(&mut self, peer_id: PeerId) -> Result<SessionDialAttempt, Error> {
        match self.dials.entry(peer_id) {
            Entry::Vacant(_) => {
                debug!(
                    target: LOG_TARGET,
                    "Received a dial error for peer {peer_id:?} that is not being tracked. \
                     This means that a new session has cleared the map of pending dials."
                );
                return Ok(SessionDialAttempt::PreviousSession);
            }
            Entry::Occupied(mut entry) => {
                let (old_dial_attempt, is_retrying) = entry.get_mut();
                if *is_retrying {
                    return Err(Error::CurrentlyRetrying);
                }
                if old_dial_attempt.attempt_number >= self.max_attempts {
                    debug!(
                        target: LOG_TARGET,
                        "Maximum attempts ({}) reached for peer {peer_id:?}. Re-dialing stopped.",
                        self.max_attempts
                    );
                    return Ok(SessionDialAttempt::OngoingSession(Some(entry.remove().0)));
                }
                let new_attempt_number = old_dial_attempt.attempt_number.checked_add(1).unwrap();
                let delay = Duration::from_secs(1 << (new_attempt_number.get() - 1));
                trace!(
                    target: LOG_TARGET,
                    "Scheduling retry {new_attempt_number} for peer {peer_id:?} in {:?} seconds.", delay.as_secs()
                );
                old_dial_attempt.attempt_number = new_attempt_number;
                *is_retrying = true;
                self.retries.push(Box::pin(async move {
                    tokio::time::sleep(delay).await;
                    peer_id
                }));
                Ok(SessionDialAttempt::OngoingSession(None))
            }
        }
    }

    pub(super) fn remove(&mut self, peer_id: &PeerId) -> Option<DialAttempt> {
        self.dials.remove(peer_id).map(|(attempt, _)| attempt)
    }

    pub(super) fn keys(&self) -> impl Iterator<Item = &PeerId> {
        self.dials.keys()
    }

    // /// Clear all active dials and pending retries (e.g., on session rotation).
    pub(super) fn clear(&mut self) {
        self.dials.clear();
        self.retries.clear();
    }

    pub(super) fn insert(&mut self, peer_id: PeerId, attempt: DialAttempt) {
        self.dials.insert(peer_id, (attempt, false));
    }

    #[cfg(test)]
    pub fn retry_count(&self) -> usize {
        self.retries.len()
    }

    #[cfg(test)]
    pub fn get(&self, peer_id: &PeerId) -> Option<&DialAttempt> {
        self.dials.get(peer_id).map(|(attempt, _)| attempt)
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.dials.is_empty()
    }
}

impl Stream for OngoingDials {
    type Item = (PeerId, DialAttempt);

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let next = Pin::new(&mut self.retries).poll_next(cx);
        match next {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(peer_id)) => {
                let Entry::Occupied(mut entry) = self.dials.entry(peer_id) else {
                    warn!(
                        target: LOG_TARGET,
                        "Received a retry signal for peer {peer_id:?} that is not being tracked. This should not happen."
                    );
                    return Poll::Pending;
                };
                entry.get_mut().1 = false;
                Poll::Ready(Some((peer_id, entry.get().0.clone())))
            }
        }
    }
}
