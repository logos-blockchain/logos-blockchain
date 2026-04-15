use core::{
    num::NonZeroU64,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use std::collections::{HashMap, hash_map::Entry};

use futures::{Stream, future::ready, stream::FuturesUnordered};
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

#[derive(Debug)]
pub enum Error {
    CurrentlyRetrying,
    AlreadyRegistered,
}

impl OngoingDials {
    pub(super) fn new(max_attempts: NonZeroU64) -> Self {
        Self {
            dials: HashMap::new(),
            retries: FuturesUnordered::new(),
            max_attempts,
        }
    }

    /// Schedule a new dial attempt for the specified peer and connection, with
    /// the provided address and message.
    ///
    /// Returns an error if a dial attempt for the specified peer and connection
    /// is already being tracked.
    pub(super) fn schedule(
        &mut self,
        key: (PeerId, ConnectionId),
        (address, message): (Multiaddr, EncapsulatedMessageWithVerifiedPublicHeader),
    ) -> Result<(), Error> {
        let Entry::Vacant(entry) = self.dials.entry(key) else {
            return Err(Error::AlreadyRegistered);
        };
        entry.insert((
            DialAttempt {
                address,
                message,
                attempt_number: 1.try_into().unwrap(),
            },
            true,
        ));
        self.retries.push(Box::pin(ready((key.0, key.1))));
        Ok(())
    }

    /// Reschedule a dial attempt for the specified peer and connection.
    ///
    /// If the dial attempt is currently being retried, an error is returned.
    /// If the maximum number of attempts has been reached, the dial attempt is
    /// removed from tracking and returned. Otherwise, the dial attempt is
    /// updated with an incremented attempt number and a new retry is scheduled.
    pub(super) fn reschedule(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
    ) -> Result<Option<DialAttempt>, Error> {
        let Entry::Occupied(mut entry) = self.dials.entry((peer_id, connection_id)) else {
            panic!(
                "Rescheduling the dial for peer {peer_id:?} and connection {connection_id:?} that is not being tracked."
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
            "Scheduling retry {new_attempt_number} for peer {peer_id:?} in {:?} seconds", delay.as_secs()     );
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

    pub(super) fn get(&self, key: &(PeerId, ConnectionId)) -> Option<&DialAttempt> {
        self.dials.get(key).map(|(attempt, _)| attempt)
    }

    #[cfg(test)]
    pub const fn active(&self) -> &HashMap<(PeerId, ConnectionId), (DialAttempt, bool)> {
        &self.dials
    }

    #[cfg(test)]
    pub fn retry_count(&self) -> usize {
        self.retries.len()
    }
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
