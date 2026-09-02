use std::collections::HashMap;

use lb_blend::message::MessageIdentifier;
use lb_chain_service::Epoch;

use crate::{LOG_TARGET, message::BlendPayload};

/// The payloads of the messages this node has built but not yet put on the
/// wire, keyed by the identifier of the layer that will go out.
///
/// A core node encapsulates a payload in one round and releases the message in
/// a later one, and by then the message is just bytes: the payload is sealed
/// inside it and the release path has no way back to it. This is that way back,
/// and it exists so that the delivery deadline can be counted from the round
/// the message actually reached the peers rather than from the round it was
/// built in — a message can wait a long time for the proofs that back it, and
/// none of that wait is time the network was given to deliver anything.
#[derive(Debug, Default)]
pub struct InFlightPayloads(HashMap<MessageIdentifier, (Epoch, BlendPayload)>);

impl InFlightPayloads {
    #[must_use]
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    /// Records that the message identified by `id`, built under `epoch`,
    /// carries `payload`.
    ///
    /// The identifier is the `PoQ` key nullifier of the layer that goes out,
    /// which is what the peers see and what no other message can repeat: a key
    /// is spent once.
    pub fn add_payload(&mut self, id: MessageIdentifier, payload: BlendPayload, epoch: Epoch) {
        assert!(
            self.0.insert(id, (epoch, payload)).is_none(),
            "Two locally-generated messages share the identifier {id:?}, which means a key was spent twice."
        );
    }

    /// Takes back the payload of a message that has just gone out, if this node
    /// is the one that built it.
    #[must_use]
    pub fn remove_payload(&mut self, id: &MessageIdentifier) -> Option<BlendPayload> {
        self.0.remove(id).map(|(_, payload)| payload)
    }

    /// Drops the block proposals an expiring epoch leaves behind.
    ///
    /// Called when an epoch's transition period ends, which is when the
    /// scheduler that could still have released its messages is dropped: a
    /// message carries the `PoQ` of the epoch it was built under, so one still
    /// queued by then is never going out and its proposal is never going to be
    /// released, let alone delivered.
    pub fn drop_expiring_epoch_proposals(&mut self, expiring: Epoch) {
        let before = self.0.len();
        self.0.retain(|_, (epoch, payload)| {
            *epoch != expiring || !matches!(payload, BlendPayload::BlockProposal(_))
        });
        let dropped = before - self.0.len();
        if dropped > 0 {
            tracing::debug!(
                target: LOG_TARGET,
                "Dropping the payloads of {dropped} block proposal(s) that epoch {expiring:?} never released."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use lb_blend::proofs::quota::VerifiedProofOfQuota;

    use super::*;

    fn id(byte: u8) -> MessageIdentifier {
        VerifiedProofOfQuota::from_bytes_unchecked([byte; _]).key_nullifier()
    }

    fn proposal() -> BlendPayload {
        BlendPayload::BlockProposal(b"proposal".to_vec())
    }

    fn transaction() -> BlendPayload {
        BlendPayload::Transaction(b"transaction".to_vec())
    }

    #[test]
    fn a_message_this_node_built_hands_its_payload_back_when_it_goes_out() {
        let mut in_flight = InFlightPayloads::new();
        in_flight.add_payload(id(1), proposal(), Epoch::new(0));

        assert_eq!(in_flight.remove_payload(&id(1)), Some(proposal()));
        assert_eq!(
            in_flight.remove_payload(&id(1)),
            None,
            "a message goes out once, and its payload is claimed once"
        );
    }

    #[test]
    fn a_message_this_node_did_not_build_carries_nothing_it_waits_on() {
        let mut in_flight = InFlightPayloads::new();
        in_flight.add_payload(id(1), proposal(), Epoch::new(0));

        assert_eq!(in_flight.remove_payload(&id(2)), None);
    }

    #[test]
    fn an_expiring_epoch_takes_its_proposals_and_leaves_its_transactions() {
        let mut in_flight = InFlightPayloads::new();
        in_flight.add_payload(id(1), proposal(), Epoch::new(0));
        in_flight.add_payload(id(2), transaction(), Epoch::new(0));
        in_flight.add_payload(id(3), proposal(), Epoch::new(1));

        in_flight.drop_expiring_epoch_proposals(Epoch::new(0));

        assert_eq!(
            in_flight.remove_payload(&id(1)),
            None,
            "a proposal is built for a slot in the epoch that just ended"
        );
        assert_eq!(
            in_flight.remove_payload(&id(2)),
            Some(transaction()),
            "a transaction is not slot-bound and outlives the epoch it waited in"
        );
        assert_eq!(
            in_flight.remove_payload(&id(3)),
            Some(proposal()),
            "another epoch's proposal is none of this one's business"
        );
    }
}
