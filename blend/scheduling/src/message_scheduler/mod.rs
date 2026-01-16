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
use rand::RngCore;
use tokio::time::interval;
use tokio_stream::wrappers::IntervalStream;
use tracing::{info, trace};

use crate::{
    cover_traffic::SessionCoverTraffic,
    message_scheduler::{
        round_info::{RoundClock, RoundInfo, RoundReleaseType},
        session_info::SessionInfo,
    },
    release_delayer::SessionProcessedMessageDelayer,
};

pub mod round_info;
pub mod session_info;

#[cfg(test)]
mod tests;

const LOG_TARGET: &str = "blend::scheduling";

/// Trait for scheduling processed messages to be released in future rounds.
pub trait ProcessedMessageScheduler<ProcessedMessage> {
    /// Add a new processed message to the release delayer component queue, for
    /// release during the next release window.
    fn schedule_processed_message(&mut self, message: ProcessedMessage);
}

/// Message scheduler that is valid only for a specific session.
pub struct SessionMessageScheduler<Rng, ProcessedMessage, DataMessage> {
    /// The module responsible for randomly generated cover messages, given the
    /// allowed session quota and accounting for data messages generated within
    /// the session.
    cover_traffic: SessionCoverTraffic<Rng, RoundClock>,
    /// The module responsible for delaying the release of processed messages
    /// that have not been fully decapsulated.
    release_delayer: SessionProcessedMessageDelayer<RoundClock, Rng, ProcessedMessage>,
    /// The queue of data messages that are stored in between rounds.
    data_messages: Vec<DataMessage>,
    /// The multi-consumer stream forked on each sub-stream.
    round_clock: RoundClock,
}

impl<Rng, ProcessedMessage, DataMessage> SessionMessageScheduler<Rng, ProcessedMessage, DataMessage>
where
    Rng: RngCore + Clone + Unpin,
    ProcessedMessage: Debug + Unpin,
    DataMessage: Debug + Unpin,
{
    pub fn new(session_info: SessionInfo, rng: Rng, settings: Settings) -> Self {
        let round_clock = Box::new(
            IntervalStream::new(interval(settings.round_duration))
                .enumerate()
                .map(|(round, _)| (round as u128).into()),
        )
        .fork();

        let cover_traffic = SessionCoverTraffic::new(
            crate::cover_traffic::Settings {
                additional_safety_intervals: settings.additional_safety_intervals,
                expected_intervals_per_session: settings.expected_intervals_per_session,
                rounds_per_interval: settings.rounds_per_interval,
                message_count: session_info
                    .core_quota
                    .div_ceil(settings.num_blend_layers.into()),
            },
            rng.clone(),
            Box::new(round_clock.clone()) as RoundClock,
        );
        let release_delayer = SessionProcessedMessageDelayer::new(
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

    pub fn rotate_session(
        self,
        new_session_info: SessionInfo,
        settings: Settings,
    ) -> (Self, OldSessionMessageScheduler<Rng, ProcessedMessage>) {
        (
            Self::new(
                new_session_info,
                self.release_delayer.rng().clone(),
                settings,
            ),
            OldSessionMessageScheduler(self.release_delayer),
        )
    }

    /// Notify the cover message submodule that a new data message has been
    /// generated in this session, which will reduce the number of cover
    /// messages generated going forward.
    pub fn queue_data_message(&mut self, message: DataMessage) {
        self.data_messages.push(message);
        self.cover_traffic.notify_new_data_message();
    }
}

impl<Rng, ProcessedMessage, DataMessage>
    SessionMessageScheduler<Rng, ProcessedMessage, DataMessage>
{
    #[cfg(test)]
    pub fn with_test_values(
        cover_traffic: SessionCoverTraffic<Rng, RoundClock>,
        release_delayer: SessionProcessedMessageDelayer<RoundClock, Rng, ProcessedMessage>,
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
    ) -> &SessionProcessedMessageDelayer<RoundClock, Rng, ProcessedMessage> {
        &self.release_delayer
    }
}

impl<Rng, ProcessedMessage, DataMessage> ProcessedMessageScheduler<ProcessedMessage>
    for SessionMessageScheduler<Rng, ProcessedMessage, DataMessage>
{
    fn schedule_processed_message(&mut self, message: ProcessedMessage) {
        self.release_delayer.schedule_message(message);
    }
}

impl<Rng, ProcessedMessage, DataMessage> Stream
    for SessionMessageScheduler<Rng, ProcessedMessage, DataMessage>
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
        info!(target: LOG_TARGET, "Emitting new round info {round_info:?}.");
        Poll::Ready(Some(round_info))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Settings {
    pub additional_safety_intervals: u64,
    pub expected_intervals_per_session: NonZeroU64,
    pub maximum_release_delay_in_rounds: NonZeroU64,
    pub round_duration: Duration,
    pub rounds_per_interval: NonZeroU64,
    pub num_blend_layers: NonZeroU64,
}

#[cfg(test)]
impl Default for Settings {
    fn default() -> Self {
        Self {
            additional_safety_intervals: 0,
            expected_intervals_per_session: NonZeroU64::try_from(1).unwrap(),
            maximum_release_delay_in_rounds: NonZeroU64::try_from(1).unwrap(),
            round_duration: Duration::from_secs(1),
            rounds_per_interval: NonZeroU64::try_from(1).unwrap(),
            num_blend_layers: NonZeroU64::try_from(1).unwrap(),
        }
    }
}

/// Message scheduler that is only for an old session during session transition.
///
/// Unlike [`SessionMessageScheduler`], this supports only scheduling processed
/// messages. Data messages cannot be scheduled, and it does not generate cover
/// messages.
pub struct OldSessionMessageScheduler<Rng, ProcessedMessage>(
    SessionProcessedMessageDelayer<RoundClock, Rng, ProcessedMessage>,
);

impl<Rng, ProcessedMessage> ProcessedMessageScheduler<ProcessedMessage>
    for OldSessionMessageScheduler<Rng, ProcessedMessage>
{
    fn schedule_processed_message(&mut self, message: ProcessedMessage) {
        self.0.schedule_message(message);
    }
}

impl<Rng, ProcessedMessage> Stream for OldSessionMessageScheduler<Rng, ProcessedMessage>
where
    Rng: rand::Rng + Unpin,
    ProcessedMessage: Unpin,
{
    type Item = Vec<ProcessedMessage>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_next_unpin(cx)
    }
}

impl<Rng, ProcessedMessage> OldSessionMessageScheduler<Rng, ProcessedMessage> {
    #[cfg(any(test, feature = "unsafe-test-functions"))]
    pub fn release_delayer(
        &self,
    ) -> &SessionProcessedMessageDelayer<RoundClock, Rng, ProcessedMessage> {
        &self.0
    }
}
