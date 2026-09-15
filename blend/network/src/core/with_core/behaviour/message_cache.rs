use std::collections::{HashMap, hash_map::Entry};

use lb_blend_message::{
    MessageIdentifier,
    encap::{
        encapsulated::EncapsulatedMessage, validated::EncapsulatedMessageWithVerifiedPublicHeader,
    },
};

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

/// Keeps track of messages that have been processed by us, in order to avoid
/// processing or forwarding the same message multiple times.
#[derive(Debug, Default)]
pub struct MessageCache {
    /// Map of message identifiers to their status.
    messages: HashMap<MessageIdentifier, MessageStatus>,
}

impl MessageCache {
    /// Creates a new `MessageCache`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark a message as processed.
    ///
    /// This means that we have received and validated the message, but we
    /// haven't forwarded it to our peers yet.
    ///
    /// The function takes an `EncapsulatedMessageWithVerifiedPublicHeader` as
    /// input because the cache is keyed by the `PoQ` key nullifier, which is
    /// only meaningful once the `PoQ` it comes from has been verified. Marking
    /// an unverified message would let anyone claim a nullifier by replaying
    /// someone else's `PoQ` under their own signing key, which suppresses the
    /// genuine message carrying it.
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
    /// input, since we want to mark the message as forwarded only after its
    /// whole public header (signature *and* `PoQ`) has been verified.
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

    /// Get the status of a message in the cache, if it exists.
    #[cfg(test)]
    pub fn message_status(&self, message_id: &MessageIdentifier) -> Option<&MessageStatus> {
        self.messages.get(message_id)
    }
}

#[cfg(test)]
mod tests {
    use lb_blend_message::encap::{
        encapsulated::EncapsulatedMessage, validated::EncapsulatedMessageWithVerifiedPublicHeader,
    };

    use crate::core::{
        tests::utils::TestEncapsulatedMessage,
        with_core::behaviour::message_cache::{MessageCache, MessageStatus},
    };

    fn make_verified(payload: &[u8]) -> EncapsulatedMessageWithVerifiedPublicHeader {
        TestEncapsulatedMessage::new(payload).into_inner()
    }

    #[test]
    fn forwarded_status_not_downgraded_to_processed() {
        let mut cache = MessageCache::new();
        let msg = make_verified(b"fw-not-downgraded");

        cache.mark_message_as_forwarded(&msg);
        cache.mark_message_as_processed(&msg);

        assert_eq!(
            cache.message_status(&msg.id()),
            Some(&MessageStatus::Forwarded),
            "Forwarded status must not be downgraded to Processed"
        );
    }

    #[test]
    fn processed_status_upgraded_to_forwarded() {
        let mut cache = MessageCache::new();
        let msg = make_verified(b"proc-upgraded");

        cache.mark_message_as_processed(&msg);
        assert_eq!(
            cache.message_status(&msg.id()),
            Some(&MessageStatus::Processed)
        );

        cache.mark_message_as_forwarded(&msg);
        assert_eq!(
            cache.message_status(&msg.id()),
            Some(&MessageStatus::Forwarded),
            "Processed status must be upgradeable to Forwarded"
        );
    }

    #[test]
    fn is_message_processed_true_for_both_statuses() {
        let mut cache = MessageCache::new();
        let proc_msg = make_verified(b"processed");
        let fwd_msg = make_verified(b"forwarded");

        cache.mark_message_as_processed(&proc_msg);
        cache.mark_message_as_forwarded(&fwd_msg);

        let raw_proc: EncapsulatedMessage = proc_msg.into();
        let raw_fwd: EncapsulatedMessage = fwd_msg.into();

        assert!(
            cache.is_message_processed(&raw_proc),
            "is_message_processed should return true for Processed status"
        );
        assert!(
            cache.is_message_processed(&raw_fwd),
            "is_message_processed should return true for Forwarded status"
        );
    }

    #[test]
    fn is_message_forwarded_returns_false_for_processed_status() {
        let mut cache = MessageCache::new();
        let msg = make_verified(b"only-processed");

        cache.mark_message_as_processed(&msg);
        let raw: EncapsulatedMessage = msg.into();

        assert!(
            !cache.is_message_forwarded(&raw),
            "is_message_forwarded should return false for Processed status"
        );
    }
}
