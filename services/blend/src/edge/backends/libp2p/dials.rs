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
use tracing::debug;

use crate::edge::backends::libp2p::LOG_TARGET;

type PendingRetries = FuturesUnordered<Pin<Box<dyn Future<Output = (PeerId, DialAttempt)> + Send>>>;

pub struct PendingDials {
    active: HashMap<(PeerId, ConnectionId), DialAttempt>,
    retries: PendingRetries,
    max_attempts: NonZeroU64,
}

impl PendingDials {
    pub(super) fn new(max_attempts: NonZeroU64) -> Self {
        Self {
            active: HashMap::new(),
            retries: FuturesUnordered::new(),
            max_attempts,
        }
    }

    pub(super) fn insert(&mut self, key: (PeerId, ConnectionId), attempt: DialAttempt) {
        self.active.insert(key, attempt);
    }

    pub(super) fn entry(
        &mut self,
        key: (PeerId, ConnectionId),
    ) -> Entry<'_, (PeerId, ConnectionId), DialAttempt> {
        self.active.entry(key)
    }

    pub(super) fn get(&self, key: &(PeerId, ConnectionId)) -> Option<&DialAttempt> {
        self.active.get(key)
    }

    pub(super) fn remove(&mut self, key: &(PeerId, ConnectionId)) -> Option<DialAttempt> {
        self.active.remove(key)
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
    ) -> Option<DialAttempt> {
        let last_dial_attempt = self.active.remove(&(peer_id, connection_id)).unwrap();
        let new_dial_attempt_number = last_dial_attempt.attempt_number.checked_add(1).unwrap();
        if new_dial_attempt_number > self.max_attempts {
            return Some(last_dial_attempt);
        }
        let delay = Duration::from_secs(1 << (new_dial_attempt_number.get() - 1));
        debug!(
            target: LOG_TARGET,
            "Scheduling retry {new_dial_attempt_number} for peer {peer_id:?} in {:?} seconds", delay.as_secs()
        );
        let new_dial_attempt = DialAttempt {
            attempt_number: new_dial_attempt_number,
            ..last_dial_attempt
        };
        self.retries.push(Box::pin(async move {
            tokio::time::sleep(delay).await;
            (peer_id, new_dial_attempt)
        }));
        None
    }

    #[cfg(test)]
    pub const fn active(&self) -> &HashMap<(PeerId, ConnectionId), DialAttempt> {
        &self.active
    }

    #[cfg(test)]
    pub fn retry_count(&self) -> usize {
        self.retries.len()
    }
}

impl Stream for PendingDials {
    type Item = (PeerId, DialAttempt);

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.retries).poll_next(cx)
    }
}

#[derive(Debug)]
pub struct DialAttempt {
    /// Address of peer being dialed.
    address: Multiaddr,
    /// The latest (ongoing) attempt number.
    attempt_number: NonZeroU64,
    /// The message to send once the peer is successfully dialed.
    message: EncapsulatedMessageWithVerifiedPublicHeader,
}

impl DialAttempt {
    pub(super) const fn new(
        address: Multiaddr,
        message: EncapsulatedMessageWithVerifiedPublicHeader,
    ) -> Self {
        Self {
            address,
            attempt_number: NonZeroU64::new(1).unwrap(),
            message,
        }
    }

    pub(super) fn into_components(
        self,
    ) -> (
        Multiaddr,
        NonZeroU64,
        EncapsulatedMessageWithVerifiedPublicHeader,
    ) {
        (self.address, self.attempt_number, self.message)
    }

    pub const fn address(&self) -> &Multiaddr {
        &self.address
    }

    pub const fn message(&self) -> &EncapsulatedMessageWithVerifiedPublicHeader {
        &self.message
    }

    #[cfg(test)]
    pub const fn attempt_number(&self) -> NonZeroU64 {
        self.attempt_number
    }
}
