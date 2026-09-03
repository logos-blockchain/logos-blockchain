use core::{
    num::NonZeroU64,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use std::collections::HashMap;

use futures::{Stream, StreamExt as _, stream::BoxStream};
use lb_blend::message::MessageIdentifier;
use lb_chain_service::Epoch;

use crate::{
    core::LOG_TARGET, delivery::FailureDetector as InnerFailureDetector, message::BlendPayload,
};

/// Wrapper around [`crate::delivery::FailureDetector`] that adds support for
/// encapsulated-but-not-yet-blended messages.
pub struct FailureDetector {
    inner: InnerFailureDetector,
    /// Payloads encapsulated and waiting to be blended.
    encapsulated: HashMap<MessageIdentifier, (Epoch, BlendPayload)>,
}

impl FailureDetector {
    #[must_use]
    pub fn new(
        maximum_blending_delay: NonZeroU64,
        round_duration: Duration,
        payload_broadcasts: BoxStream<'static, BlendPayload>,
    ) -> Self {
        Self {
            inner: InnerFailureDetector::new(
                maximum_blending_delay,
                round_duration,
                payload_broadcasts,
            ),
            encapsulated: HashMap::new(),
        }
    }

    /// Register the link between a payload and its outermost encapsulation, so
    /// that when the encapsulated message is actually released, we can record
    /// what payload it belongs to.
    pub fn mark_payload_as_encapsulated(
        &mut self,
        id: MessageIdentifier,
        payload: BlendPayload,
        epoch: Epoch,
    ) {
        assert!(
            self.encapsulated.insert(id, (epoch, payload)).is_none(),
            "Two locally-generated messages share the identifier {id:?}, which means a key was spent twice."
        );
    }

    /// Mark a previously-recorded encapsulated payload as released at this
    /// round.
    ///
    /// An `id` that was never recorded is ignored rather than refused: the
    /// release path reports every message it puts on the wire and lets this map
    /// pick out the ones that were ours, so most of what arrives here is
    /// traffic this node only relays, or messages a previous run left in the
    /// recovery state. Claiming a payload twice is what cannot happen, and
    /// `remove` is what stops it.
    pub fn mark_encapsulated_payload_as_released(&mut self, id: MessageIdentifier) {
        if let Some((_, payload)) = self.encapsulated.remove(&id) {
            self.inner.mark_payload_as_blended(payload);
        }
    }

    /// Drops the block proposals an expiring epoch leaves behind.
    pub fn drop_proposals_for_epoch(&mut self, expiring: Epoch) {
        let before = self.encapsulated.len();
        self.encapsulated.retain(|_, (epoch, payload)| {
            *epoch != expiring || !matches!(payload, BlendPayload::BlockProposal(_))
        });
        let dropped = before.checked_sub(self.encapsulated.len()).unwrap();
        if dropped > 0 {
            tracing::debug!(
                target: LOG_TARGET,
                "Dropping the payloads of {dropped} block proposal(s) that epoch {expiring:?} never released."
            );
        }
    }

    /// How many payloads are waiting for the Blend network to deliver them.
    #[cfg(test)]
    #[must_use]
    pub fn outstanding_payloads_count(&self) -> usize {
        self.inner.outstanding_payloads_count()
    }
}

impl Stream for FailureDetector {
    type Item = <InnerFailureDetector as Stream>::Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.poll_next_unpin(cx)
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt as _;
    use lb_blend::{message::MessageIdentifier, proofs::quota::VerifiedProofOfQuota};
    use lb_chain_service::Epoch;
    use tokio::{sync::mpsc, time::Instant};
    use tokio_stream::wrappers::UnboundedReceiverStream;

    use crate::{
        core::delivery::FailureDetector,
        delivery::test_utils::{DEADLINE, ROUND, proposal, transaction, until},
        message::BlendPayload,
    };

    /// Rounds a message spends waiting on the proofs that back it, before it is
    /// released.
    const HELD_FOR: u64 = 3;

    /// A core sender, the instant its round clock started, and the handle that
    /// puts payloads on the broadcasting channel it watches.
    fn new_failure_monitor() -> (
        FailureDetector,
        Instant,
        mpsc::UnboundedSender<BlendPayload>,
    ) {
        let (channel, broadcasts) = mpsc::unbounded_channel();
        let detection = FailureDetector::new(
            DEADLINE,
            ROUND,
            UnboundedReceiverStream::new(broadcasts).boxed(),
        );
        (detection, Instant::now(), channel)
    }

    fn id(byte: u8) -> MessageIdentifier {
        VerifiedProofOfQuota::from_bytes_unchecked([byte; _]).key_nullifier()
    }

    #[tokio::test(start_paused = true)]
    async fn a_released_payload_the_network_never_delivers_is_broadcast_at_the_deadline() {
        let (mut detection, start, _channel) = new_failure_monitor();
        detection.mark_payload_as_encapsulated(id(1), proposal(), Epoch::new(0));
        detection.mark_encapsulated_payload_as_released(id(1));

        assert!(
            until(&mut detection, start, DEADLINE.get() - 2)
                .await
                .is_empty(),
            "revealing a proposal the network is still delivering is the one thing the deadline exists to prevent"
        );
        assert_eq!(
            until(&mut detection, start, DEADLINE.get() + 2).await,
            vec![proposal()]
        );
        assert_eq!(
            detection.outstanding_payloads_count(),
            0,
            "it is broadcast exactly once"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_deadline_runs_from_the_release_and_not_from_the_encapsulation() {
        let (mut detection, start, _channel) = new_failure_monitor();
        // Built now, but held back — waiting on the proofs that will back it.
        detection.mark_payload_as_encapsulated(id(1), proposal(), Epoch::new(0));
        assert!(until(&mut detection, start, HELD_FOR).await.is_empty());
        detection.mark_encapsulated_payload_as_released(id(1));

        assert!(
            until(&mut detection, start, DEADLINE.get() + 1)
                .await
                .is_empty(),
            "the wait for proofs is not time the network was given to deliver anything"
        );
        assert_eq!(
            until(&mut detection, start, DEADLINE.get() + HELD_FOR + 2).await,
            vec![proposal()]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_message_this_node_only_relays_is_nothing_it_waits_on() {
        let (mut detection, start, _channel) = new_failure_monitor();
        detection.mark_encapsulated_payload_as_released(id(9));

        assert_eq!(detection.outstanding_payloads_count(), 0);
        assert!(
            until(&mut detection, start, DEADLINE.get() + 3)
                .await
                .is_empty()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_expiring_epoch_takes_its_proposals_and_leaves_its_transactions() {
        let (mut detection, ..) = new_failure_monitor();
        detection.mark_payload_as_encapsulated(id(1), proposal(), Epoch::new(0));
        detection.mark_payload_as_encapsulated(id(2), transaction(), Epoch::new(0));
        detection.mark_payload_as_encapsulated(id(3), proposal(), Epoch::new(1));

        detection.drop_proposals_for_epoch(Epoch::new(0));
        for released in [id(1), id(2), id(3)] {
            detection.mark_encapsulated_payload_as_released(released);
        }

        assert_eq!(
            detection.outstanding_payloads_count(),
            2,
            "the expiring epoch's proposal is gone, its transaction and the next epoch's proposal are not"
        );
    }
}
