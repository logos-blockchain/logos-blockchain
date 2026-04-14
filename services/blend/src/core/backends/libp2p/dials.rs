use core::{
    num::NonZeroU64,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use std::collections::HashMap;

use futures::{Stream, stream::FuturesUnordered};
use libp2p::{Multiaddr, PeerId};
use tracing::debug;

use crate::core::backends::libp2p::LOG_TARGET;

pub struct DialAttempt {
    /// Address of peer being dialed.
    address: Multiaddr,
    /// The latest (ongoing) attempt number.
    attempt_number: NonZeroU64,
}

impl DialAttempt {
    pub(super) const fn new(address: Multiaddr) -> Self {
        Self {
            address,
            attempt_number: NonZeroU64::new(1).unwrap(),
        }
    }

    pub(super) const fn address(&self) -> &Multiaddr {
        &self.address
    }

    pub(super) const fn increment_attempt(&mut self) {
        self.attempt_number = self.attempt_number.checked_add(1).unwrap();
    }

    #[cfg(test)]
    pub const fn attempt_number(&self) -> NonZeroU64 {
        self.attempt_number
    }
}

/// [`DialAttempt`] with session information, i.e., whether the attempt was made
/// at this session or the previous one.
pub enum SessionDialAttempt {
    OngoingSession(Option<DialAttempt>),
    PreviousSession,
}

type PendingRetries = FuturesUnordered<Pin<Box<dyn Future<Output = (PeerId, DialAttempt)> + Send>>>;

pub struct OngoingDials {
    active: HashMap<PeerId, DialAttempt>,
    retries: PendingRetries,
    max_attempts: NonZeroU64,
}

impl OngoingDials {
    pub(super) fn new(max_attempts: NonZeroU64, capacity: usize) -> Self {
        Self {
            active: HashMap::with_capacity(capacity),
            retries: FuturesUnordered::new(),
            max_attempts,
        }
    }

    pub(super) fn remove(&mut self, peer_id: &PeerId) -> Option<DialAttempt> {
        self.active.remove(peer_id)
    }

    pub(super) fn keys(&self) -> impl Iterator<Item = &PeerId> {
        self.active.keys()
    }

    /// Clear all active dials and pending retries (e.g., on session rotation).
    pub(super) fn clear(&mut self) {
        self.active.clear();
        self.retries.clear();
    }

    /// Insert or bump the attempt counter for a peer, returning the address
    /// to be used for the dial. The caller is responsible for actually
    /// calling `swarm.dial()`.
    pub(super) fn insert_or_bump(&mut self, peer_id: PeerId, address: Multiaddr) -> &Multiaddr {
        use std::collections::hash_map::Entry;
        match self.active.entry(peer_id) {
            Entry::Vacant(empty_entry) => {
                let attempt = empty_entry.insert(DialAttempt::new(address));
                &attempt.address
            }
            Entry::Occupied(mut existing_entry) => {
                existing_entry.get_mut().increment_attempt();
                &existing_entry.into_mut().address
            }
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
    pub(super) fn schedule_retry(&mut self, peer_id: PeerId) -> SessionDialAttempt {
        let Some(dial_attempt) = self.active.remove(&peer_id) else {
            debug!(
                target: LOG_TARGET,
                "Received a dial error for peer {peer_id:?} that is not being tracked. \
                 This means that a new session has cleared the map of pending dials."
            );
            return SessionDialAttempt::PreviousSession;
        };
        if dial_attempt.attempt_number >= self.max_attempts {
            debug!(
                target: LOG_TARGET,
                "Maximum attempts ({}) reached for peer {peer_id:?}. Re-dialing stopped.",
                self.max_attempts
            );
            return SessionDialAttempt::OngoingSession(Some(dial_attempt));
        }
        let new_attempt_number = dial_attempt.attempt_number.checked_add(1).unwrap();
        let delay = Duration::from_secs(1 << (new_attempt_number.get() - 1));
        debug!(
            target: LOG_TARGET,
            "Scheduling retry {new_attempt_number} for peer {peer_id:?} in {delay:?}"
        );
        let new_dial_attempt = DialAttempt {
            address: dial_attempt.address,
            attempt_number: new_attempt_number,
        };
        self.retries.push(Box::pin(async move {
            tokio::time::sleep(delay).await;
            (peer_id, new_dial_attempt)
        }));
        SessionDialAttempt::OngoingSession(None)
    }

    #[cfg(test)]
    pub fn retry_count(&self) -> usize {
        self.retries.len()
    }

    #[cfg(test)]
    pub fn get(&self, peer_id: &PeerId) -> Option<&DialAttempt> {
        self.active.get(peer_id)
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }
}

impl Stream for OngoingDials {
    type Item = (PeerId, DialAttempt);

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.retries).poll_next(cx)
    }
}
