use core::{mem::take, num::NonZeroU64};
use std::collections::HashMap;

use lb_blend::scheduling::message_scheduler::round_info::Round;

use crate::message::BlendPayload;

/// The payloads this node released into the Blend network that have not come
/// out of it yet, each against the round it last went out in.
///
/// Keyed by the payload itself: the set has to hold it anyway, to broadcast it
/// if it comes to that, and it is the payload that comes back on the
/// broadcasting channel.
pub struct PendingDeliveries {
    maximum_delay_allowed: NonZeroU64,
    outstanding: HashMap<BlendPayload, Round>,
}

impl PendingDeliveries {
    #[must_use]
    pub fn new(maximum_delay_allowed: NonZeroU64) -> Self {
        Self {
            maximum_delay_allowed,
            outstanding: HashMap::new(),
        }
    }

    /// Records that a message carrying `payload` went out to the peers in
    /// `round`, which is when its deadline starts.
    pub fn mark_payload_as_released(&mut self, payload: BlendPayload, round: Round) {
        self.outstanding.insert(payload, round);
    }

    /// Records what the broadcasting channel has just carried, which clears
    /// whatever of this node's own was waiting for it.
    ///
    /// Almost nothing that arrives here matches: a node sees every proposal and
    /// every transaction the whole network puts out, and only a handful of them
    /// are ever its own. A miss is the ordinary case rather than an anomaly,
    /// which is why it is silent.
    pub fn mark_payload_as_delivered(&mut self, payload: &BlendPayload) {
        self.outstanding.remove(payload);
    }

    /// The payloads whose deadline has passed by `now`, dropped from the set:
    /// the caller broadcasts them directly and nothing waits on them any more.
    #[must_use]
    pub fn get_expired_payloads_at_round(&mut self, now: Round) -> Vec<BlendPayload> {
        let deadline = self.maximum_delay_allowed;
        let (expired, still_waiting) =
            take(&mut self.outstanding)
                .into_iter()
                .partition(|(_, released_at)| {
                    released_at
                        .inner()
                        .saturating_add(u128::from(deadline.get()))
                        <= now.inner()
                });
        self.outstanding = still_waiting;
        expired.into_keys().collect()
    }

    /// How many payloads are still waiting for the network to deliver them.
    #[must_use]
    pub fn outstanding_count(&self) -> usize {
        self.outstanding.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEADLINE: NonZeroU64 = NonZeroU64::new(6).unwrap();

    fn pending() -> PendingDeliveries {
        PendingDeliveries::new(DEADLINE)
    }

    fn proposal(body: &[u8]) -> BlendPayload {
        BlendPayload::BlockProposal(body.to_vec())
    }

    #[test]
    fn a_payload_the_network_never_delivers_is_handed_back_at_the_deadline() {
        let mut pending = pending();
        pending.mark_payload_as_released(proposal(b"lost"), Round::from(0));

        assert!(
            pending
                .get_expired_payloads_at_round(Round::from(5))
                .is_empty(),
            "revealing a proposal the network is still delivering is the one thing the deadline exists to prevent"
        );
        assert_eq!(
            pending.get_expired_payloads_at_round(Round::from(6)),
            vec![proposal(b"lost")]
        );
        assert_eq!(
            pending.outstanding_count(),
            0,
            "it is handed back exactly once"
        );
        assert!(
            pending
                .get_expired_payloads_at_round(Round::from(60))
                .is_empty()
        );
    }

    #[test]
    fn a_payload_seen_on_the_broadcasting_channel_is_never_handed_back() {
        let mut pending = pending();
        let delivered = proposal(b"delivered");
        pending.mark_payload_as_released(delivered.clone(), Round::from(0));
        pending.mark_payload_as_delivered(&delivered);

        assert_eq!(pending.outstanding_count(), 0);
        assert!(
            pending
                .get_expired_payloads_at_round(Round::from(60))
                .is_empty(),
            "the deadline passing means nothing once the proposal is on the channel"
        );
    }

    #[test]
    fn a_transaction_does_not_answer_for_a_proposal_of_the_same_bytes() {
        let mut pending = pending();
        pending.mark_payload_as_released(proposal(b"same"), Round::from(0));
        pending.mark_payload_as_delivered(&BlendPayload::Transaction(b"same".to_vec()));

        assert_eq!(
            pending.outstanding_count(),
            1,
            "what a payload is, is part of what identifies it"
        );
    }

    #[test]
    fn the_last_copy_released_is_the_one_whose_deadline_decides() {
        let mut pending = pending();
        pending.mark_payload_as_released(proposal(b"two copies"), Round::from(0));
        pending.mark_payload_as_released(proposal(b"two copies"), Round::from(3));

        assert!(
            pending
                .get_expired_payloads_at_round(Round::from(8))
                .is_empty(),
            "a proposal is lost only once every copy carrying it has failed"
        );
        assert_eq!(
            pending.get_expired_payloads_at_round(Round::from(9)),
            vec![proposal(b"two copies")]
        );
        assert_eq!(pending.outstanding_count(), 0);
    }

    #[test]
    fn everything_released_in_one_round_expires_together() {
        let mut pending = pending();
        pending.mark_payload_as_released(proposal(b"one"), Round::from(0));
        pending
            .mark_payload_as_released(BlendPayload::Transaction(b"two".to_vec()), Round::from(0));

        assert_eq!(
            pending.get_expired_payloads_at_round(Round::from(6)).len(),
            2
        );
        assert_eq!(pending.outstanding_count(), 0);
    }
}
