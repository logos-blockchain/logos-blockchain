use core::{
    fmt::Debug,
    mem::take,
    num::NonZeroU64,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use fork_stream::StreamExt as _;
use futures::{Stream, StreamExt as _};
use lb_log_targets::blend;
use rand::RngCore;
use tokio::time::{MissedTickBehavior, interval};
use tokio_stream::wrappers::IntervalStream;
use tracing::trace;

use crate::{
    cover_traffic::EpochCoverTraffic,
    message_scheduler::{
        epoch_info::EpochInfo,
        round_info::{RoundClock, RoundInfo, RoundReleaseType},
    },
    release_delayer::EpochProcessedMessageDelayer,
};

pub mod epoch_info;
pub mod round_info;

#[cfg(test)]
mod tests;

const LOG_TARGET: &str = blend::scheduling::ROOT;

/// Labels distinguishing the two schedulers in the round traces, since both are
/// polled on every round for the duration of an epoch transition.
const CURRENT_EPOCH: &str = "current epoch";
const OLD_EPOCH: &str = "old epoch";

/// The round-advancing logic shared by the current- and old-epoch schedulers.
///
/// Advances `round_clock`, then asks `poll_release_type` what is due for
/// release this round: `Poll::Ready(None)` to end the stream, `Poll::Pending`
/// for nothing, or `Poll::Ready(Some(_))` for the release type. A round is
/// emitted whenever there is anything to send, which includes the case where
/// the only thing pending is a queued data message.
fn poll_round_info<ProcessedMessage, DataMessage>(
    epoch: &str,
    round_clock: &mut RoundClock,
    data_messages: &mut Vec<DataMessage>,
    cx: &mut Context<'_>,
    poll_release_type: impl FnOnce(&mut Context<'_>) -> Poll<Option<RoundReleaseType<ProcessedMessage>>>,
) -> Poll<Option<RoundInfo<ProcessedMessage, DataMessage>>>
where
    ProcessedMessage: Debug,
    DataMessage: Debug,
{
    // We do not return anything if a new round has not elapsed.
    let new_round = match round_clock.poll_next_unpin(cx) {
        Poll::Pending => return Poll::Pending,
        Poll::Ready(None) => return Poll::Ready(None),
        Poll::Ready(Some(new_round)) => new_round,
    };
    trace!(target: LOG_TARGET, "New round {new_round} started for the {epoch}.");

    // Determine the release type without consuming `data_messages` yet, so the
    // early-return arms below cannot drop already-taken data messages.
    let release_type = match poll_release_type(cx) {
        Poll::Ready(None) => return Poll::Ready(None),
        // Nothing else is due at this round, so we return `Ready` only if we have data
        // messages to release. Else, we return `Pending`.
        Poll::Pending => {
            if data_messages.is_empty() {
                // Awake to trigger a new round clock tick.
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            None
        }
        Poll::Ready(Some(release_type)) => Some(release_type),
    };

    // Safe to take now: every path from here on emits the data messages.
    let round_info = RoundInfo {
        data_messages: take(data_messages),
        release_type,
    };
    trace!(
        target: LOG_TARGET,
        data_messages = round_info.data_messages.len(),
        release_type = ?round_info.release_type,
        "emitting new round info for the {epoch}"
    );
    Poll::Ready(Some(round_info))
}

/// Trait for scheduling processed messages to be released in future rounds.
pub trait ProcessedMessageScheduler<ProcessedMessage> {
    /// Add a new processed message to the release delayer component queue, for
    /// release during the next release window.
    fn schedule_processed_message(&mut self, message: ProcessedMessage);
}

/// Message scheduler that is valid only for a specific epoch.
pub struct EpochMessageScheduler<Rng, ProcessedMessage, DataMessage> {
    /// The module responsible for randomly generated cover messages, given the
    /// allowed epoch quota and accounting for data messages generated within
    /// the epoch.
    cover_traffic: EpochCoverTraffic<Rng, RoundClock>,
    /// The module responsible for delaying the release of processed messages
    /// that have not been fully decapsulated.
    release_delayer: EpochProcessedMessageDelayer<RoundClock, Rng, ProcessedMessage>,
    /// The queue of data messages that are stored in between rounds.
    data_messages: Vec<DataMessage>,
    /// The multi-consumer stream forked on each sub-stream.
    round_clock: RoundClock,
}

impl<Rng, ProcessedMessage, DataMessage> EpochMessageScheduler<Rng, ProcessedMessage, DataMessage>
where
    Rng: RngCore + Clone + Unpin,
    ProcessedMessage: Debug + Unpin,
    DataMessage: Debug + Unpin,
{
    pub fn new(epoch_info: EpochInfo, rng: Rng, settings: Settings) -> Self {
        let interval = {
            let mut interval = interval(settings.round_duration);
            interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

            interval
        };
        let round_clock = Box::new(
            IntervalStream::new(interval)
                .enumerate()
                .map(|(round, _)| (round as u128).into()),
        )
        .fork();

        let cover_traffic = EpochCoverTraffic::new(
            crate::cover_traffic::Settings {
                rounds_per_epoch: settings.rounds_per_epoch,
                // Floor division: each cover message consumes `num_blend_layers`
                // proofs from a hard cap of `core_quota`. Using `div_ceil` would
                // schedule one extra emission whenever the quota is not an exact
                // multiple of the layer count, and that last emission would fail
                // with `NoMoreProofOfQuotas`. Flooring keeps the scheduled count
                // within what the quota can actually satisfy.
                message_count: epoch_info.core_quota.get() / u64::from(settings.num_blend_layers),
            },
            rng.clone(),
            Box::new(round_clock.clone()) as RoundClock,
        );
        let release_delayer = EpochProcessedMessageDelayer::new(
            crate::release_delayer::Settings {
                maximum_release_delay_in_rounds: settings.maximum_release_delay_in_rounds,
            },
            rng,
            Box::new(round_clock.clone()) as RoundClock,
        );

        Self {
            cover_traffic,
            release_delayer,
            data_messages: Vec::new(),
            round_clock: Box::new(round_clock) as RoundClock,
        }
    }

    pub fn consume(self) -> OldEpochMessageScheduler<Rng, ProcessedMessage, DataMessage> {
        let Self {
            release_delayer,
            data_messages,
            round_clock,
            ..
        } = self;
        OldEpochMessageScheduler {
            release_delayer,
            data_messages,
            round_clock,
        }
    }

    pub fn rotate_epoch(
        self,
        new_epoch_info: EpochInfo,
        settings: Settings,
    ) -> (
        Self,
        OldEpochMessageScheduler<Rng, ProcessedMessage, DataMessage>,
    ) {
        // Data messages queued but not released before the epoch ended stay with the
        // old epoch's scheduler, alongside the processed messages it already drains,
        // so each epoch's scheduler owns that epoch's traffic. They were encapsulated
        // with the old epoch's `PoQ`, which only verifies against that epoch's public
        // inputs: carrying them into the new epoch would publish them under the new
        // epoch's number, and the receiver would reject the proof and close us as a
        // spammer. They go out to the old epoch's peers instead, and whatever is
        // still queued when the transition period expires is dropped with them.
        let new_scheduler = Self::new(new_epoch_info, self.release_delayer.rng().clone(), settings);
        (new_scheduler, self.consume())
    }

    /// Notify the cover message submodule that a new data message has been
    /// generated in this epoch, which will reduce the number of cover
    /// messages generated going forward.
    pub fn queue_data_message(&mut self, message: DataMessage) {
        self.data_messages.push(message);
        self.cover_traffic.notify_new_data_message();
    }
}

impl<Rng, ProcessedMessage, DataMessage> EpochMessageScheduler<Rng, ProcessedMessage, DataMessage> {
    #[cfg(test)]
    pub fn with_test_values(
        cover_traffic: EpochCoverTraffic<Rng, RoundClock>,
        release_delayer: EpochProcessedMessageDelayer<RoundClock, Rng, ProcessedMessage>,
        round_clock: RoundClock,
        data_messages: Vec<DataMessage>,
    ) -> Self {
        Self {
            cover_traffic,
            release_delayer,
            data_messages,
            round_clock,
        }
    }

    #[cfg(any(test, feature = "unsafe-test-functions"))]
    pub fn release_delayer(
        &self,
    ) -> &EpochProcessedMessageDelayer<RoundClock, Rng, ProcessedMessage> {
        &self.release_delayer
    }
}

impl<Rng, ProcessedMessage, DataMessage> ProcessedMessageScheduler<ProcessedMessage>
    for EpochMessageScheduler<Rng, ProcessedMessage, DataMessage>
{
    fn schedule_processed_message(&mut self, message: ProcessedMessage) {
        self.release_delayer.schedule_message(message);
    }
}

impl<Rng, ProcessedMessage, DataMessage> Stream
    for EpochMessageScheduler<Rng, ProcessedMessage, DataMessage>
where
    Rng: rand::Rng + Clone + Unpin,
    ProcessedMessage: Debug + Unpin,
    DataMessage: Debug + Unpin,
{
    type Item = RoundInfo<ProcessedMessage, DataMessage>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let Self {
            cover_traffic,
            release_delayer,
            round_clock,
            data_messages,
        } = &mut *self;

        poll_round_info(CURRENT_EPOCH, round_clock, data_messages, cx, |cx| {
            // We poll the sub-streams and return the right result accordingly. Both are
            // polled unconditionally so that both register their waker.
            let cover_traffic_output = cover_traffic.poll_next_unpin(cx);
            let release_delayer_output = release_delayer.poll_next_unpin(cx);

            match (cover_traffic_output, release_delayer_output) {
                // Bubble up `Poll::Ready(None)` if any sub-stream returns it.
                (Poll::Ready(None), _) | (_, Poll::Ready(None)) => Poll::Ready(None),
                // Neither sub-stream is ready, so only queued data messages are due.
                (Poll::Pending, Poll::Pending) => Poll::Pending,
                // Cover message, no processed messages.
                (Poll::Ready(Some(())), Poll::Pending) => {
                    Poll::Ready(Some(RoundReleaseType::OnlyCoverMessage))
                }
                // Processed messages, no cover message.
                (Poll::Pending, Poll::Ready(Some(processed_messages))) => Poll::Ready(Some(
                    RoundReleaseType::OnlyProcessedMessages(processed_messages),
                )),
                // Cover and processed messages.
                (Poll::Ready(Some(())), Poll::Ready(Some(processed_messages))) => {
                    Poll::Ready(Some(RoundReleaseType::ProcessedAndCoverMessages(
                        processed_messages,
                    )))
                }
            }
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Settings {
    pub maximum_release_delay_in_rounds: NonZeroU64,
    pub round_duration: Duration,
    pub rounds_per_epoch: NonZeroU64,
    pub num_blend_layers: NonZeroU64,
}

#[cfg(test)]
impl Default for Settings {
    fn default() -> Self {
        Self {
            maximum_release_delay_in_rounds: NonZeroU64::try_from(1).unwrap(),
            round_duration: Duration::from_secs(1),
            rounds_per_epoch: NonZeroU64::try_from(1).unwrap(),
            num_blend_layers: NonZeroU64::try_from(1).unwrap(),
        }
    }
}

/// Message scheduler that is only for an old epoch during epoch transition.
///
/// It drains what the epoch left behind: the processed messages held by its
/// release delayer, and the data messages that had been queued but not released
/// when the epoch ended. No new data message can be queued on it, and it does
/// not generate cover messages, so the release type it yields is never a cover
/// one. Whatever is still queued when the transition period expires is dropped
/// along with this scheduler.
pub struct OldEpochMessageScheduler<Rng, ProcessedMessage, DataMessage> {
    /// The module responsible for delaying the release of processed messages
    /// that have not been fully decapsulated.
    release_delayer: EpochProcessedMessageDelayer<RoundClock, Rng, ProcessedMessage>,
    /// The data messages the old epoch had queued but not yet released.
    data_messages: Vec<DataMessage>,
    /// The multi-consumer stream forked on each sub-stream.
    round_clock: RoundClock,
}

impl<Rng, ProcessedMessage, DataMessage> ProcessedMessageScheduler<ProcessedMessage>
    for OldEpochMessageScheduler<Rng, ProcessedMessage, DataMessage>
{
    fn schedule_processed_message(&mut self, message: ProcessedMessage) {
        self.release_delayer.schedule_message(message);
    }
}

impl<Rng, ProcessedMessage, DataMessage> Stream
    for OldEpochMessageScheduler<Rng, ProcessedMessage, DataMessage>
where
    Rng: rand::Rng + Unpin,
    ProcessedMessage: Debug + Unpin,
    DataMessage: Debug + Unpin,
{
    type Item = RoundInfo<ProcessedMessage, DataMessage>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let Self {
            release_delayer,
            data_messages,
            round_clock,
        } = &mut *self;

        poll_round_info(OLD_EPOCH, round_clock, data_messages, cx, |cx| {
            // The old epoch generates no cover traffic, so the only thing that can come
            // due besides the leftover data messages is a batch of processed messages.
            release_delayer
                .poll_next_unpin(cx)
                .map(|messages| messages.map(RoundReleaseType::OnlyProcessedMessages))
        })
    }
}

impl<Rng, ProcessedMessage, DataMessage>
    OldEpochMessageScheduler<Rng, ProcessedMessage, DataMessage>
{
    #[cfg(any(test, feature = "unsafe-test-functions"))]
    pub fn release_delayer(
        &self,
    ) -> &EpochProcessedMessageDelayer<RoundClock, Rng, ProcessedMessage> {
        &self.release_delayer
    }
}
