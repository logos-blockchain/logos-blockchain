use std::collections::{HashMap, HashSet, hash_map::Entry};

use lb_blend_message::{
    MessageIdentifier,
    encap::{
        encapsulated::EncapsulatedMessage, validated::EncapsulatedMessageWithVerifiedPublicHeader,
    },
};
use libp2p::PeerId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MessageStatus {
    /// Message has been received and validated, but not yet forwarded to
    /// connected peers.
    Processed,
    /// Message has been forwarded to connected peers, so it won't be forwarded
    /// again nor processed if received.
    Forwarded,
}

/// Keeps track of messages that have been processed by us, and messages that we
/// have seen from our peers, in order to avoid processing or forwarding the
/// same message multiple times.
#[derive(Debug, Default)]
pub struct MessageCache {
    /// Map of message identifiers to whether we have only received or send and
    /// received them.
    messages: HashMap<MessageIdentifier, MessageStatus>,
    /// Map of peer identifiers to the set of message identifiers that we have
    /// seen from that peer, to be used when considering whether a peer is
    /// malicious by sending duplicate messages.
    received_from_peers: HashMap<PeerId, HashSet<MessageIdentifier>>,
}

impl MessageCache {
    /// Creates a new `MessageCache` with default capacity for the number of
    /// peers that we expect to receive messages from.
    #[cfg(test)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a new `MessageCache` with the given capacity for the number of
    /// peers that we expect to receive messages from.
    pub fn new_with_peer_capacity(capacity: usize) -> Self {
        Self {
            messages: HashMap::new(),
            received_from_peers: HashMap::with_capacity(capacity),
        }
    }

    /// Mark a message with the given identifier as received.
    ///
    /// This function does not keep into account whether we already registered a
    /// message as seen from a peer, but only whether the message was already
    /// processed by us or not.
    pub fn mark_message_as_processed(
        &mut self,
        message: &EncapsulatedMessageWithVerifiedPublicHeader,
    ) {
        // Processed messages are also considered as received, so we only mark the
        // message as received if it is not already marked as processed.
        let Entry::Vacant(entry) = self.messages.entry(message.id()) else {
            return;
        };
        entry.insert(MessageStatus::Processed);
    }

    /// Mark a message with the given identifier as processed, meaning we won't
    /// allow the swarm to send any duplicates of it.
    pub fn mark_message_as_forwarded(
        &mut self,
        message: &EncapsulatedMessageWithVerifiedPublicHeader,
    ) {
        self.messages.insert(message.id(), MessageStatus::Forwarded);
    }

    /// Check whether a message with the given identifier has already been
    /// received by us, meaning that we won't bubble it up to the swarm
    /// again.
    pub fn is_message_processed(&self, message: &EncapsulatedMessage) -> bool {
        self.messages.contains_key(&message.id())
    }

    /// Check whether a message with the given identifier has already been
    /// processed (i.e. validated and forwarded) by us.
    pub fn is_message_forwarded(&self, message: &EncapsulatedMessage) -> bool {
        matches!(
            self.messages.get(&message.id()),
            Some(MessageStatus::Forwarded)
        )
    }

    /// Mark a message with the given identifier as seen from the given peer,
    /// and return whether it was the first time we marked it as such for
    /// that peer.
    pub fn mark_message_as_seen_from_peer(
        &mut self,
        message: &EncapsulatedMessage,
        peer_id: PeerId,
    ) -> bool {
        self.received_from_peers
            .entry(peer_id)
            .or_default()
            .insert(message.id())
    }

    /// Remove all the messages seen from the given peer.
    pub fn remove_peer_info(&mut self, peer_id: &PeerId) {
        self.received_from_peers.remove(peer_id);
    }

    /// Get an iterator over the message identifiers of the messages that we
    /// have seen from the given peer.
    #[cfg(test)]
    pub fn messages_from_peer(&self, peer_id: &PeerId) -> impl Iterator<Item = MessageIdentifier> {
        self.received_from_peers
            .get(peer_id)
            .into_iter()
            .flat_map(|set| set.iter().copied())
    }

    #[cfg(test)]
    pub fn message_status(&self, message_id: &MessageIdentifier) -> Option<&MessageStatus> {
        self.messages.get(message_id)
    }
}
