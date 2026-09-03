use core::{
    future::poll_fn,
    hash::Hash,
    mem::take,
    num::NonZeroU64,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use std::collections::HashMap;

use futures::{
    Stream, StreamExt as _,
    stream::{BoxStream, Fuse},
};
use lb_blend::scheduling::message_scheduler::round_info::Round;
use tokio::time::{MissedTickBehavior, interval};
use tokio_stream::wrappers::IntervalStream;

use crate::{
    LOG_TARGET, core::network::NetworkAdapter, delivery::broadcast_undelivered_messages,
    message::NetworkMessage,
};

struct BlendedPayloadDetails {
    released_at: Round,
    broadcasted: bool,
}

pub struct FailureDetector<BroadcastSettings> {
    maximum_blending_delay: NonZeroU64,
    rounds_clock: IntervalStream,
    current_round: Round,
    payload_broadcasts: Fuse<BoxStream<'static, NetworkMessage<BroadcastSettings>>>,
    /// Messages sent out via Blend, up to the round their deadline passes.
    unacknowledged_blended_payloads:
        HashMap<NetworkMessage<BroadcastSettings>, BlendedPayloadDetails>,
}

impl<BroadcastSettings> FailureDetector<BroadcastSettings>
where
    BroadcastSettings: Eq + Hash,
{
    #[must_use]
    pub fn new(
        maximum_blending_delay: NonZeroU64,
        round_duration: Duration,
        payload_broadcasts: BoxStream<'static, NetworkMessage<BroadcastSettings>>,
    ) -> Self {
        let clock = {
            let mut clock = interval(round_duration);
            clock.set_missed_tick_behavior(MissedTickBehavior::Skip);
            clock
        };
        Self {
            maximum_blending_delay,
            rounds_clock: IntervalStream::new(clock),
            current_round: Round::from(0),
            payload_broadcasts: payload_broadcasts.fuse(),
            unacknowledged_blended_payloads: HashMap::new(),
        }
    }

    pub fn mark_payload_as_blended(&mut self, payload: NetworkMessage<BroadcastSettings>) {
        let released_at = self.current_round;
        self.unacknowledged_blended_payloads
            .entry(payload)
            // Overwrite the release time if the payload is blended again (in case of multiple
            // copies), so that the last copy released is the one whose deadline
            // decides.
            .and_modify(|blended| blended.released_at = released_at)
            .or_insert(BlendedPayloadDetails {
                released_at,
                broadcasted: false,
            });
    }

    fn mark_payload_as_delivered(&mut self, payload: &NetworkMessage<BroadcastSettings>) {
        if let Some(blended) = self.unacknowledged_blended_payloads.get_mut(payload) {
            blended.broadcasted = true;
        }
    }

    fn take_expired_payloads(&mut self, now: Round) -> Vec<NetworkMessage<BroadcastSettings>> {
        let (expired, still_waiting): (HashMap<_, _>, HashMap<_, _>) =
            take(&mut self.unacknowledged_blended_payloads)
                .into_iter()
                .partition(|(_, blended)| {
                    blended
                        .released_at
                        .inner()
                        .checked_add(u128::from(self.maximum_blending_delay.get()))
                        .expect("Round calculation overflow.")
                        < now.inner()
                });
        self.unacknowledged_blended_payloads = still_waiting;
        expired
            .into_iter()
            // Only yield the ones that have not already been seen in the meanwhile.
            .filter_map(|(payload, blended)| (!blended.broadcasted).then_some(payload))
            .collect()
    }

    #[cfg(test)]
    #[must_use]
    pub fn outstanding_payloads_count(&self) -> usize {
        self.unacknowledged_blended_payloads.len()
    }
}

impl<BroadcastSettings> FailureDetector<BroadcastSettings>
where
    BroadcastSettings: Eq + Hash + Send + Unpin,
{
    /// Waits out the delivery deadlines this node still owes, broadcasting in
    /// the clear each payload whose deadline expires.
    pub async fn drain_pending_message_queue<NetAdapter, RuntimeServiceId>(
        mut self,
        network_adapter: &NetAdapter,
    ) where
        NetAdapter: NetworkAdapter<RuntimeServiceId, BroadcastSettings = BroadcastSettings> + Sync,
    {
        loop {
            let expired = poll_fn(|cx| {
                if self.unacknowledged_blended_payloads.is_empty() {
                    return Poll::Ready(None);
                }
                match self.poll_next_unpin(cx) {
                    Poll::Ready(expired) => Poll::Ready(expired),
                    Poll::Pending if self.unacknowledged_blended_payloads.is_empty() => {
                        Poll::Ready(None)
                    }
                    Poll::Pending => Poll::Pending,
                }
            })
            .await;
            let Some(undelivered) = expired else {
                return;
            };
            broadcast_undelivered_messages(undelivered.into_iter(), network_adapter).await;
        }
    }
}

impl<BroadcastSettings> Stream for FailureDetector<BroadcastSettings>
where
    BroadcastSettings: Eq + Hash + Unpin,
{
    type Item = Vec<NetworkMessage<BroadcastSettings>>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            // Take what the broadcasting channel has for us before looking at the clock,
            // so that a payload delivered in this very round is never the one the same
            // round reveals.
            loop {
                match self.payload_broadcasts.poll_next_unpin(cx) {
                    Poll::Pending => break,
                    Poll::Ready(Some(delivered)) => self.mark_payload_as_delivered(&delivered),
                    Poll::Ready(None) => {
                        tracing::error!(
                            target: LOG_TARGET,
                            "Lost sight of the broadcasting channel; a delivery can no longer be told from a loss, so the direct broadcast is disabled for the rest of this run."
                        );
                        return Poll::Ready(None);
                    }
                }
            }
            match self.rounds_clock.poll_next_unpin(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Ready(Some(_)) => {
                    let now = Round::from(
                        self.current_round
                            .inner()
                            .checked_add(1)
                            .expect("Round computation overflow."),
                    );
                    self.current_round = now;
                    let expired = self.take_expired_payloads(now);
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
    use futures::{StreamExt as _, stream};
    use tokio::{sync::mpsc, time::Instant};
    use tokio_stream::wrappers::UnboundedReceiverStream;

    use crate::{
        delivery::{
            FailureDetector,
            test_utils::{DEADLINE, ROUND, another_payload, proposal, until},
        },
        message::NetworkMessage,
    };

    /// An edge sender, the instant its round clock started, and the handle that
    /// puts payloads on the broadcasting channel it watches.
    fn watching() -> (
        FailureDetector<()>,
        Instant,
        mpsc::UnboundedSender<NetworkMessage<()>>,
    ) {
        let (channel, broadcasts) = mpsc::unbounded_channel();
        let detection = FailureDetector::new(
            DEADLINE,
            ROUND,
            UnboundedReceiverStream::new(broadcasts).boxed(),
        );
        (detection, Instant::now(), channel)
    }

    #[tokio::test(start_paused = true)]
    async fn a_payload_the_network_never_delivers_is_broadcast_at_the_deadline() {
        let (mut detection, start, _channel) = watching();
        detection.mark_payload_as_blended(proposal());

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

    /// The deadline is a count of rounds, but what it owes a payload is a
    /// duration, and a payload is recorded against the round already in
    /// progress. Counting from that round number alone would spend whatever of
    /// it had already elapsed before the payload was ever released.
    #[tokio::test(start_paused = true)]
    async fn a_payload_is_never_revealed_before_its_full_deadline_has_elapsed() {
        let (mut detection, start, _channel) = watching();
        // Half a round in: the worst case for a count of rounds, and the case an
        // inclusive comparison against the round number gets wrong.
        let released_at = start + ROUND * 2 + ROUND / 2;
        let _turned = until(&mut detection, start, 2).await;
        tokio::time::sleep_until(released_at).await;
        detection.mark_payload_as_blended(proposal());

        let rounds = u32::try_from(DEADLINE.get()).expect("The deadline is a few rounds.");
        let expired = tokio::time::timeout(ROUND * (rounds + 3), detection.next())
            .await
            .expect("the deadline must expire eventually")
            .expect("an interval never ends, so neither does the detector");

        assert_eq!(expired, vec![proposal()]);
        assert!(
            released_at.elapsed() >= ROUND * rounds,
            "revealing a proposal before the network has had its full deadline is the one thing the deadline exists to prevent"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_delivered_payload_is_never_broadcast_in_the_clear() {
        let (mut detection, start, channel) = watching();
        detection.mark_payload_as_blended(proposal());
        channel.send(proposal()).expect("the watch is listening");

        assert!(
            until(&mut detection, start, DEADLINE.get() + 3)
                .await
                .is_empty(),
            "the deadline passing means nothing once the proposal is on the channel"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_last_copy_released_is_the_one_whose_deadline_decides() {
        /// Rounds between the two copies going out.
        const APART: u64 = 3;

        let (mut detection, start, _channel) = watching();
        detection.mark_payload_as_blended(proposal());
        assert!(until(&mut detection, start, APART).await.is_empty());
        detection.mark_payload_as_blended(proposal());

        assert!(
            until(&mut detection, start, DEADLINE.get() + 1)
                .await
                .is_empty(),
            "a proposal is lost only once every copy carrying it has failed"
        );
        assert_eq!(
            until(&mut detection, start, DEADLINE.get() + APART + 2).await,
            vec![proposal()]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn everything_released_in_one_round_expires_together() {
        let (mut detection, start, _channel) = watching();
        detection.mark_payload_as_blended(proposal());
        detection.mark_payload_as_blended(another_payload());

        assert_eq!(
            until(&mut detection, start, DEADLINE.get() + 2).await.len(),
            2
        );
        assert_eq!(detection.outstanding_payloads_count(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_round_in_which_nothing_expires_does_not_wake_the_loop() {
        // The channel is held open: an idle sender is one that can still see the
        // broadcasting channel, not one that has lost it.
        let (mut detection, _start, _channel) = watching();
        assert!(
            tokio::time::timeout(ROUND * 40, detection.next())
                .await
                .is_err(),
            "an idle sender must not wake its service loop every round"
        );
    }

    /// The broadcasting channel is the only evidence of delivery there is.
    /// Without it every payload looks lost, so carrying on would answer a
    /// broken observer by revealing everything this node sends — the
    /// opposite of what the fallback is for.
    #[tokio::test(start_paused = true)]
    async fn losing_sight_of_the_broadcasting_channel_stops_the_detection() {
        let mut detection = FailureDetector::new(DEADLINE, ROUND, stream::empty().boxed());
        let start = Instant::now();
        detection.mark_payload_as_blended(proposal());

        assert!(
            until(&mut detection, start, DEADLINE.get() + 2)
                .await
                .is_empty(),
            "a node that cannot see the channel must not answer that by revealing its own traffic"
        );
        assert_eq!(
            detection.next().await,
            None,
            "the branch this feeds is disabled outright rather than polled forever"
        );
    }

    /// With replication a proposal goes out as two messages, and the second may
    /// be released after the first has already been seen delivered. Gossipsub
    /// drops the duplicate, so no second observation is coming to cancel a
    /// deadline the second release would start.
    #[tokio::test(start_paused = true)]
    async fn a_copy_released_after_a_delivery_is_not_revealed() {
        let (mut detection, start, channel) = watching();
        detection.mark_payload_as_blended(proposal());
        channel.send(proposal()).expect("the watch is listening");
        // Let the delivery land before the second copy goes out.
        assert!(until(&mut detection, start, 2).await.is_empty());

        detection.mark_payload_as_blended(proposal());

        assert!(
            until(&mut detection, start, DEADLINE.get() + 4)
                .await
                .is_empty(),
            "the proposal is on the chain; a later copy of it is not a reason to reveal it"
        );
    }
}
