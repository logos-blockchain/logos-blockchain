use core::{
    num::NonZeroU64,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use futures::{Stream, StreamExt as _, stream::BoxStream};
use lb_blend::{message::MessageIdentifier, scheduling::message_scheduler::round_info::Round};
use lb_chain_service::Epoch;
use tokio::time::{MissedTickBehavior, interval};
use tokio_stream::wrappers::IntervalStream;

use crate::{
    delivery::{in_flight::InFlightPayloads, pending::PendingDeliveries},
    message::BlendPayload,
};

/// What a sender knows about the payloads it has handed to the Blend network,
/// or nothing at all if the operator has turned the direct broadcast off.
///
/// It is a stream of the payloads whose deadline passed with no delivery, which
/// is exactly the set its owner has to broadcast in the clear. A round in which
/// nothing expires yields nothing rather than an empty batch, so the service
/// loop only wakes when there is a broadcast to make — and a node that is not
/// watching yields nothing ever, which disables the branch outright.
pub enum DeliveryLogic {
    /// Watching for its own payloads on the broadcasting channel, and ready to
    /// broadcast one in the clear when the deadline passes without it.
    Watching(Box<Watch>),
    /// Blending and nothing else: nothing is recorded, no channel is watched,
    /// and no deadline can fire.
    Blended,
}

/// The two halves of watching, which are the two things a sender never sees at
/// once: a message on the wire is bytes with its payload sealed inside it, and
/// a payload on the broadcasting channel says nothing about which message
/// carried it. So one half holds payloads against the messages that will carry
/// them, and the other holds them against the round they went out in.
pub struct Watch {
    /// The round clock is this type's own rather than the message scheduler's.
    /// A scheduler belongs to one epoch and starts counting again with the
    /// next, whereas a proposal released in the last round of an epoch has to
    /// be watched through the rotation; and an edge node, which has no
    /// scheduler at all, waits out the same deadline as a core node.
    clock: IntervalStream,
    round: Round,
    /// The Logos Blockchain broadcasting channel, as this node sees it.
    broadcasts: BoxStream<'static, BlendPayload>,
    in_flight: InFlightPayloads,
    pending: PendingDeliveries,
}

impl DeliveryLogic {
    /// Watches `broadcasts` for the payloads this node releases, and hands back
    /// the ones still missing `deadline` rounds later.
    #[must_use]
    pub fn watching(
        deadline: NonZeroU64,
        round_duration: Duration,
        broadcasts: BoxStream<'static, BlendPayload>,
    ) -> Self {
        let mut clock = interval(round_duration);
        // The deadline counts rounds, so a round the runtime was too busy to deliver on
        // time is one round however late it arrives; catching up on missed ticks would
        // spend several rounds' worth of deadline in an instant.
        clock.set_missed_tick_behavior(MissedTickBehavior::Skip);
        Self::Watching(Box::new(Watch {
            clock: IntervalStream::new(clock),
            round: Round::from(0),
            broadcasts,
            in_flight: InFlightPayloads::new(),
            pending: PendingDeliveries::new(deadline),
        }))
    }

    /// Does not watch, and so never reveals anything.
    #[must_use]
    pub const fn blended() -> Self {
        Self::Blended
    }

    const fn watch_mut(&mut self) -> Option<&mut Watch> {
        match self {
            Self::Watching(watch) => Some(watch),
            Self::Blended => None,
        }
    }

    /// Records that a message this node has built under `epoch` carries
    /// `payload`, against the round in which that message goes out.
    ///
    /// A core node encapsulates in one round and releases in a later one, and
    /// the deadline belongs to the later of the two: a message can wait a long
    /// time for the proofs that back it, and none of that wait is time the
    /// network was given to deliver anything.
    pub fn mark_payload_as_in_flight(
        &mut self,
        id: MessageIdentifier,
        payload: BlendPayload,
        epoch: Epoch,
    ) {
        if let Some(watch) = self.watch_mut() {
            watch.in_flight.add_payload(id, payload, epoch);
        }
    }

    /// Records that the message identified by `id` has gone out to the peers,
    /// which starts the deadline of whatever it carries.
    ///
    /// Every message the node releases is reported, its own and the ones it
    /// merely relays: a relayed one carries nothing this node is waiting on and
    /// is ignored, which is what lets the release path report all of them
    /// without having to tell them apart.
    pub fn mark_scheduled_payload_as_released(&mut self, id: MessageIdentifier) {
        if let Some(watch) = self.watch_mut()
            && let Some(payload) = watch.in_flight.remove_payload(&id)
        {
            let round = watch.round;
            watch.pending.mark_payload_as_released(payload, round);
        }
    }

    /// Records a message that goes out the moment it is built, carrying
    /// `payload` — which is what an edge node does, having no release schedule
    /// to wait for.
    pub fn mark_payload_as_released(&mut self, payload: BlendPayload) {
        if let Some(watch) = self.watch_mut() {
            let round = watch.round;
            watch.pending.mark_payload_as_released(payload, round);
        }
    }

    pub fn drop_expiring_epoch_proposals(&mut self, expiring: Epoch) {
        if let Some(watch) = self.watch_mut() {
            watch.in_flight.drop_expiring_epoch_proposals(expiring);
        }
    }

    /// How many payloads are waiting for the Blend network to deliver them.
    #[must_use]
    pub fn outstanding(&self) -> usize {
        match self {
            Self::Watching(watch) => watch.pending.outstanding_count(),
            Self::Blended => 0,
        }
    }
}

impl Stream for DeliveryLogic {
    type Item = Vec<BlendPayload>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let Some(watch) = self.watch_mut() else {
            // Nothing is recorded, so nothing can expire, and the branch this
            // feeds is disabled for good rather than polled every time round.
            return Poll::Ready(None);
        };
        loop {
            // Take what the broadcasting channel has for us before looking at the
            // clock, so that a payload delivered in this very round is never the one
            // the same round reveals.
            while let Poll::Ready(Some(delivered)) = watch.broadcasts.poll_next_unpin(cx) {
                watch.pending.mark_payload_as_delivered(&delivered);
            }
            match watch.clock.poll_next_unpin(cx) {
                Poll::Pending => return Poll::Pending,
                // An interval never ends, so neither does this.
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Ready(Some(_)) => {
                    let now = Round::from(watch.round.inner().saturating_add(1));
                    watch.round = now;
                    let expired = watch.pending.get_expired_payloads_at_round(now);
                    if !expired.is_empty() {
                        return Poll::Ready(Some(expired));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use futures::stream;
    use lb_blend::proofs::quota::VerifiedProofOfQuota;
    use tokio::{sync::mpsc, time::Instant};
    use tokio_stream::wrappers::UnboundedReceiverStream;

    use super::*;

    const ROUND: Duration = Duration::from_secs(1);
    const DEADLINE: NonZeroU64 = NonZeroU64::new(6).unwrap();
    /// Rounds a message spends waiting on the proofs that back it, before it is
    /// released.
    const HELD_FOR: u64 = 3;

    /// A tracker, the instant its round clock started, and the handle that puts
    /// payloads on the broadcasting channel it watches.
    fn watching() -> (DeliveryLogic, Instant, mpsc::UnboundedSender<BlendPayload>) {
        let (channel, broadcasts) = mpsc::unbounded_channel();
        let deliveries = DeliveryLogic::watching(
            DEADLINE,
            ROUND,
            UnboundedReceiverStream::new(broadcasts).boxed(),
        );
        (deliveries, Instant::now(), channel)
    }

    fn id(byte: u8) -> MessageIdentifier {
        VerifiedProofOfQuota::from_bytes_unchecked([byte; _]).key_nullifier()
    }

    fn proposal() -> BlendPayload {
        BlendPayload::BlockProposal(b"proposal".to_vec())
    }

    /// Turns the clock until `round`, collecting whatever expired on the way.
    /// Rounds are counted from `start` rather than from now, so a test that
    /// calls this several times does not accumulate an offset.
    async fn until(
        deliveries: &mut DeliveryLogic,
        start: Instant,
        round: u64,
    ) -> Vec<BlendPayload> {
        let stop = start + ROUND * u32::try_from(round).expect("The tests turn few rounds.");
        let mut expired = Vec::new();
        let _timed_out = tokio::time::timeout_at(stop, async {
            while let Some(batch) = deliveries.next().await {
                expired.extend(batch);
            }
        })
        .await;
        expired
    }

    #[tokio::test(start_paused = true)]
    async fn a_payload_the_network_never_delivers_is_broadcast_at_the_deadline() {
        let (mut deliveries, start, _channel) = watching();
        deliveries.mark_payload_as_in_flight(id(1), proposal(), Epoch::new(0));
        deliveries.mark_scheduled_payload_as_released(id(1));

        assert!(
            until(&mut deliveries, start, DEADLINE.get() - 2)
                .await
                .is_empty(),
            "revealing a proposal the network is still delivering is the one thing the deadline exists to prevent"
        );
        assert_eq!(
            until(&mut deliveries, start, DEADLINE.get() + 2).await,
            vec![proposal()]
        );
        assert_eq!(deliveries.outstanding(), 0, "it is broadcast exactly once");
    }

    #[tokio::test(start_paused = true)]
    async fn the_deadline_runs_from_the_release_and_not_from_the_encapsulation() {
        let (mut deliveries, start, _channel) = watching();
        // Built now, but held back — waiting on the proofs that will back it.
        deliveries.mark_payload_as_in_flight(id(1), proposal(), Epoch::new(0));
        assert!(until(&mut deliveries, start, HELD_FOR).await.is_empty());
        deliveries.mark_scheduled_payload_as_released(id(1));

        assert!(
            until(&mut deliveries, start, DEADLINE.get() + 1)
                .await
                .is_empty(),
            "the wait for proofs is not time the network was given to deliver anything"
        );
        assert_eq!(
            until(&mut deliveries, start, DEADLINE.get() + HELD_FOR + 2).await,
            vec![proposal()]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_message_this_node_only_relays_is_nothing_it_waits_on() {
        let (mut deliveries, start, _channel) = watching();
        deliveries.mark_scheduled_payload_as_released(id(9));

        assert_eq!(deliveries.outstanding(), 0);
        assert!(
            until(&mut deliveries, start, DEADLINE.get() + 3)
                .await
                .is_empty()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_delivered_payload_is_never_broadcast_in_the_clear() {
        let (mut deliveries, start, channel) = watching();
        deliveries.mark_payload_as_in_flight(id(1), proposal(), Epoch::new(0));
        deliveries.mark_scheduled_payload_as_released(id(1));
        channel.send(proposal()).expect("the watch is listening");

        assert!(
            until(&mut deliveries, start, DEADLINE.get() + 3)
                .await
                .is_empty(),
            "the deadline passing means nothing once the proposal is on the channel"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_edge_sender_releases_what_it_encapsulates() {
        let (mut deliveries, start, _channel) = watching();
        deliveries.mark_payload_as_released(proposal());

        assert_eq!(
            until(&mut deliveries, start, DEADLINE.get() + 2).await,
            vec![proposal()]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_round_in_which_nothing_expires_does_not_wake_the_loop() {
        let (mut deliveries, _, _channel) = watching();
        assert!(
            tokio::time::timeout(ROUND * 40, deliveries.next())
                .await
                .is_err(),
            "an idle sender must not wake its service loop every round"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_node_that_does_not_bypass_records_nothing_and_reveals_nothing() {
        let mut deliveries = DeliveryLogic::blended();
        deliveries.mark_payload_as_in_flight(id(1), proposal(), Epoch::new(0));
        deliveries.mark_scheduled_payload_as_released(id(1));
        deliveries.mark_payload_as_released(proposal());

        assert_eq!(deliveries.outstanding(), 0);
        assert_eq!(
            deliveries.next().await,
            None,
            "the branch this feeds is disabled outright rather than polled forever"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_broadcasting_channel_that_ends_does_not_stop_the_deadline() {
        let mut deliveries = DeliveryLogic::watching(DEADLINE, ROUND, stream::empty().boxed());
        let start = Instant::now();
        deliveries.mark_payload_as_released(proposal());

        assert_eq!(
            until(&mut deliveries, start, DEADLINE.get() + 2).await,
            vec![proposal()]
        );
    }
}
