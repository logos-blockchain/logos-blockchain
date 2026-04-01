use std::collections::{HashMap, HashSet};

use lb_blend_message::MessageIdentifier;
use libp2p::PeerId;

/// Keeps track of messages that have been processed by us, and messages that we
/// have seen from our peers, in order to avoid processing or forwarding the
/// same message multiple times.
#[derive(Debug, Default)]
pub struct MessageCache {
    /// Set of message identifiers that have been processed by us, and that we
    /// should not process or forward again.
    processed_messages: HashSet<MessageIdentifier>,
    /// Map of peer identifiers to the set of message identifiers that we have
    /// seen from that peer, to be used when considering whether a peer is
    /// malicious by sending duplicate messages.
    received_messages: HashMap<PeerId, HashSet<MessageIdentifier>>,
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
            processed_messages: HashSet::new(),
            received_messages: HashMap::with_capacity(capacity),
        }
    }

    /// Mark a message with the given identifier as processed, and return
    /// whether it was the first time we marked it as such.
    ///
    /// This function does not keep into account whether we already registered a
    /// message as seen from a peer, but only whether the message was already
    /// processed by us or not.
    pub fn mark_message_as_processed(&mut self, message_id: MessageIdentifier) -> bool {
        self.processed_messages.insert(message_id)
    }

    /// Check whether a message with the given identifier has already been
    /// processed by us.
    pub fn is_message_processed(&self, message_id: &MessageIdentifier) -> bool {
        self.processed_messages.contains(message_id)
    }

    /// Mark a message with the given identifier as seen from the given peer,
    /// and return whether it was the first time we marked it as such for
    /// that peer.
    pub fn mark_message_as_seen_from_peer(
        &mut self,
        message_id: MessageIdentifier,
        peer_id: PeerId,
    ) -> bool {
        self.received_messages
            .entry(peer_id)
            .or_default()
            .insert(message_id)
    }

    /// Remove all the messages seen from the given peer.
    pub fn remove_peer_info(&mut self, peer_id: &PeerId) {
        self.received_messages.remove(peer_id);
    }

    /// Get an iterator over the message identifiers of the messages that we
    /// have seen from the given peer.
    #[cfg(test)]
    pub fn messages_from_peer(&self, peer_id: &PeerId) -> impl Iterator<Item = MessageIdentifier> {
        self.received_messages
            .get(peer_id)
            .into_iter()
            .flat_map(|set| set.iter().copied())
    }
}
