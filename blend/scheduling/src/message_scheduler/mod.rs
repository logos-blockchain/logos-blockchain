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
                message_count: epoch_info
                    .core_quota
                    .div_ceil(settings.num_blend_layers.into()),
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

    pub fn consume(self) -> OldEpochMessageScheduler<Rng, ProcessedMessage> {
        OldEpochMessageScheduler(self.release_delayer)
    }

    pub fn rotate_epoch(
        self,
        new_epoch_info: EpochInfo,
        settings: Settings,
    ) -> (Self, OldEpochMessageScheduler<Rng, ProcessedMessage>) {
        (
            Self::new(new_epoch_info, self.release_delayer.rng().clone(), settings),
            OldEpochMessageScheduler(self.release_delayer),
        )
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

        // We do not return anything if a new round has not elapsed.
        let new_round = match round_clock.poll_next_unpin(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(None) => return Poll::Ready(None),
            Poll::Ready(Some(new_round)) => new_round,
        };
        trace!(target: LOG_TARGET, "New round {new_round} started.");
        let data_messages_to_release = take(data_messages);

        // We poll the sub-stream and return the right result accordingly.
        let cover_traffic_output = cover_traffic.poll_next_unpin(cx);
        let release_delayer_output = release_delayer.poll_next_unpin(cx);

        let round_info = match (
            cover_traffic_output,
            release_delayer_output,
            data_messages_to_release,
        ) {
            // If none of the sub-streams is ready, we return `Ready` if we have data messages to
            // release at this round. Else, we return `Pending`.
            (Poll::Pending, Poll::Pending, data_messages) => {
                if data_messages.is_empty() {
                    // Awake to trigger a new round clock tick.
                    cx.waker().wake_by_ref();
                    return Poll::Pending;
                }
                RoundInfo {
                    data_messages,
                    release_type: None,
                }
            }
            // Bubble up `Poll::Ready(None)` if any sub-stream returns it.
            (Poll::Ready(None), _, _) | (_, Poll::Ready(None), _) => return Poll::Ready(None),
            // Data and cover messages, no processed messages.
            (Poll::Ready(Some(())), Poll::Pending, data_messages) => RoundInfo {
                data_messages,
                release_type: Some(RoundReleaseType::OnlyCoverMessage),
            },
            // Data and processed messages, no cover message.
            (Poll::Pending, Poll::Ready(Some(processed_messages)), data_messages) => RoundInfo {
                data_messages,
                release_type: Some(RoundReleaseType::OnlyProcessedMessages(processed_messages)),
            },
            // Data, cover, and processed messages.
            (Poll::Ready(Some(())), Poll::Ready(Some(processed_messages)), data_messages) => {
                RoundInfo {
                    data_messages,
                    release_type: Some(RoundReleaseType::ProcessedAndCoverMessages(
                        processed_messages,
                    )),
                }
            }
        };
        trace!(
            target: LOG_TARGET,
            data_messages = round_info.data_messages.len(),
            release_type = ?round_info.release_type,
            "emitting new round info"
        );
        Poll::Ready(Some(round_info))
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
/// Unlike [`EpochMessageScheduler`], this supports only scheduling processed
/// messages. Data messages cannot be scheduled, and it does not generate cover
/// messages.
pub struct OldEpochMessageScheduler<Rng, ProcessedMessage>(
    EpochProcessedMessageDelayer<RoundClock, Rng, ProcessedMessage>,
);

impl<Rng, ProcessedMessage> ProcessedMessageScheduler<ProcessedMessage>
    for OldEpochMessageScheduler<Rng, ProcessedMessage>
{
    fn schedule_processed_message(&mut self, message: ProcessedMessage) {
        self.0.schedule_message(message);
    }
}

impl<Rng, ProcessedMessage> Stream for OldEpochMessageScheduler<Rng, ProcessedMessage>
where
    Rng: rand::Rng + Unpin,
    ProcessedMessage: Unpin,
{
    type Item = Vec<ProcessedMessage>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_next_unpin(cx)
    }
}

impl<Rng, ProcessedMessage> OldEpochMessageScheduler<Rng, ProcessedMessage> {
    #[cfg(any(test, feature = "unsafe-test-functions"))]
    pub fn release_delayer(
        &self,
    ) -> &EpochProcessedMessageDelayer<RoundClock, Rng, ProcessedMessage> {
        &self.0
    }
}
