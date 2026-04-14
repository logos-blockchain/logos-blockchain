use core::{
    num::NonZeroU64,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use std::collections::{HashMap, hash_map::Entry};

use futures::{Stream, stream::FuturesUnordered};
use lb_blend::message::encap::validated::EncapsulatedMessageWithVerifiedPublicHeader;
use libp2p::{Multiaddr, PeerId, swarm::ConnectionId};
use tracing::{debug, error, trace, warn};

use crate::edge::backends::libp2p::LOG_TARGET;

type PendingRetries =
    FuturesUnordered<Pin<Box<dyn Future<Output = (PeerId, ConnectionId)> + Send>>>;

pub struct OngoingDials {
    dials: HashMap<(PeerId, ConnectionId), (DialAttempt, bool)>,
    retries: PendingRetries,
    max_attempts: NonZeroU64,
}

pub enum Error {
    CurrentlyRetrying,
}

impl OngoingDials {
    pub(super) fn new(max_attempts: NonZeroU64) -> Self {
        Self {
            dials: HashMap::new(),
            retries: FuturesUnordered::new(),
            max_attempts,
        }
    }

    /// Attempt to retry dialing the specified peer, if the maximum attempts
    /// have not already been performed.
    ///
    /// Returns `None` if a new retry is scheduled, `Some` otherwise
    /// with the dial details of the peer that has exhausted its retries.
    ///
    /// Retries use exponential backoff: attempt 2 waits 2s, attempt 3 waits
    /// 4s, attempt N waits 2^(N-1) seconds.
    pub(super) fn schedule_retry(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
    ) -> Result<Option<DialAttempt>, Error> {
        let Entry::Occupied(mut entry) = self.dials.entry((peer_id, connection_id)) else {
            panic!(
                "Received a dial error for peer {peer_id:?} and connection {connection_id:?} that is not being tracked."
            );
        };
        let (old_dial_attempt, is_retrying) = entry.get_mut();
        if *is_retrying {
            return Err(Error::CurrentlyRetrying);
        }
        if old_dial_attempt.attempt_number >= self.max_attempts {
            return Ok(Some(entry.remove().0));
        }
        let new_attempt_number = old_dial_attempt.attempt_number.checked_add(1).unwrap();
        let delay = Duration::from_secs(1 << (new_attempt_number.get() - 1));
        trace!(
            target: LOG_TARGET,
            "Scheduling retry {new_attempt_number} for peer {peer_id:?} in {:?} seconds", delay.as_secs()
        );
        old_dial_attempt.attempt_number = new_attempt_number;
        *is_retrying = true;
        self.retries.push(Box::pin(async move {
            tokio::time::sleep(delay).await;
            (peer_id, connection_id)
        }));
        Ok(None)
    }

    pub(super) fn remove(&mut self, key: &(PeerId, ConnectionId)) -> Option<DialAttempt> {
        self.dials.remove(key).map(|(attempt, _)| attempt)
    }

    pub(super) fn insert(&mut self, key: (PeerId, ConnectionId), attempt: DialAttempt) {
        self.dials.insert(key, (attempt, false));
    }

    // pub(super) fn entry(
    //     &mut self,
    //     key: (PeerId, ConnectionId),
    // ) -> Entry<'_, (PeerId, ConnectionId), DialAttempt> {
    //     self.dials.entry(key)
    // }

    pub(super) fn get(&self, key: &(PeerId, ConnectionId)) -> Option<&DialAttempt> {
        self.dials.get(key).map(|(attempt, _)| attempt)
    }

    // #[cfg(test)]
    // pub const fn active(&self) -> &HashMap<(PeerId, ConnectionId), DialAttempt> {
    //     &self.dials
    // }

    // #[cfg(test)]
    // pub fn retry_count(&self) -> usize {
    //     self.retries.len()
    // }
}

impl Stream for OngoingDials {
    type Item = (PeerId, DialAttempt);

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let next = Pin::new(&mut self.retries).poll_next(cx);
        match next {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some((peer_id, connection_id))) => {
                let Entry::Occupied(mut entry) = self.dials.entry((peer_id, connection_id)) else {
                    warn!(
                        target: LOG_TARGET,
                        "Received a retry signal for peer {peer_id:?} and connection {connection_id:?} that is not being tracked. This should not happen."
                    );
                    return Poll::Pending;
                };
                entry.get_mut().1 = false;
                Poll::Ready(Some((peer_id, entry.get().0.clone())))
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct DialAttempt {
    /// Address of peer being dialed.
    pub address: Multiaddr,
    /// The latest (ongoing) attempt number.
    pub attempt_number: NonZeroU64,
    /// The message to send once the peer is successfully dialed.
    pub message: EncapsulatedMessageWithVerifiedPublicHeader,
}
