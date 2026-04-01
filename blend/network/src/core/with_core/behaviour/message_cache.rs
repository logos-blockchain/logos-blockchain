use std::collections::{HashMap, HashSet, hash_map::Entry};

use lb_blend_message::{
    MessageIdentifier,
    encap::{
        encapsulated::EncapsulatedMessage, validated::EncapsulatedMessageWithVerifiedPublicHeader,
    },
};
use libp2p::PeerId;

/// Status of a message in the cache.
///
/// It can be either `Processed`, meaning that we have received and validated
/// the message, but we haven't forwarded it to our peers yet, or `Forwarded`,
/// meaning that we have already forwarded the message to our peers.
///
/// A message can move into the `Forwarded` state in one of two cases:
/// - If we receive a message that we haven't seen before, we mark it as
///   `Processed`, and then we forward it to our peers, marking it as
///   `Forwarded` after forwarding it.
/// - If we receive a message to forward from Blend service, we mark it as
///   `Forwarded` immediately, since we won't forward it again nor process the
///   same message if received from our peers.
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
    /// Map of message identifiers to their status.
    messages: HashMap<MessageIdentifier, MessageStatus>,
    /// Map of peer identifiers to the set of message identifiers that we have
    /// seen from that peer, to be used when considering whether a peer is
    /// malicious by sending duplicate messages.
    received_from_peers: HashMap<PeerId, HashSet<MessageIdentifier>>,
}

impl MessageCache {
    /// Creates a new `MessageCache`.
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

    /// Mark a message as processed.
    ///
    /// This means that we have received and validated the message, but we
    /// haven't forwarded it to our peers yet.
    ///
    /// The function takes an `EncapsulatedMessageWithVerifiedPublicHeader` as
    /// input, since we want to mark the message as processed only after
    /// validating it.
    pub fn mark_message_as_processed(
        &mut self,
        message: &EncapsulatedMessageWithVerifiedPublicHeader,
    ) {
        // Forwarded messages are also considered received (i.e. we ignore them if we
        // receive them later on), so we only mark the message as received if it
        // is not already marked as processed.
        let Entry::Vacant(entry) = self.messages.entry(message.id()) else {
            return;
        };
        entry.insert(MessageStatus::Processed);
    }

    /// Mark a message as forwarded, meaning we won't allow the swarm to send
    /// any duplicates of it, nor process it if received from our peers.
    ///
    /// The function takes an `EncapsulatedMessageWithVerifiedPublicHeader` as
    /// input, since we want to mark the message as forwarded only after
    /// validating it.
    pub fn mark_message_as_forwarded(
        &mut self,
        message: &EncapsulatedMessageWithVerifiedPublicHeader,
    ) {
        self.messages.insert(message.id(), MessageStatus::Forwarded);
    }

    /// Check whether a message has already been processed by us, meaning that
    /// we won't bubble it up to the swarm again. Forwarded messages are also
    /// considered processed, so they will be included in the check.
    ///
    /// The function takes an `EncapsulatedMessage` as input, since we want to
    /// check for duplicates before doing any expensive work validating the
    /// message, since the message ID won't change before and after validation.
    pub fn is_message_processed(&self, message: &EncapsulatedMessage) -> bool {
        self.messages.contains_key(&message.id())
    }

    /// Check whether a message has already been forwarded by us.
    ///
    /// The function takes an `EncapsulatedMessage` as input, since we want to
    /// check for duplicates before doing any expensive work validating the
    /// message, since the message ID won't change before and after validation.
    pub fn is_message_forwarded(&self, message: &EncapsulatedMessage) -> bool {
        matches!(
            self.messages.get(&message.id()),
            Some(MessageStatus::Forwarded)
        )
    }

    /// Mark a message as seen from the given peer, and return whether it was
    /// the first time we marked it as such for that peer.
    ///
    /// The function takes an `EncapsulatedMessage` as input, since we want to
    /// check for duplicates before doing any expensive work validating the
    /// message, since the message ID won't change before and after validation.
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

    /// Get the status of a message in the cache, if it exists.
    #[cfg(test)]
    pub fn message_status(&self, message_id: &MessageIdentifier) -> Option<&MessageStatus> {
        self.messages.get(message_id)
    }
}
