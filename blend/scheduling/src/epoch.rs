use std::{
    future::Future as _,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use futures::StreamExt as _;
use tokio::time::{Sleep, sleep};

use crate::stream::{FirstReadyStreamError, UninitializedFirstReadyStream};

/// A staging type that initializes a [`EpochEventStream`] by consuming
/// the first [`Event`] from the underlying stream, expected to be yielded
/// within a short timeout.
pub struct UninitializedEpochEventStream<EventStream> {
    stream: UninitializedFirstReadyStream<EventStream>,
    transition_period: Duration,
}

impl<EventStream> UninitializedEpochEventStream<EventStream> {
    #[must_use]
    pub const fn new(event_stream: EventStream, transition_period: Duration) -> Self {
        Self {
            stream: UninitializedFirstReadyStream::new(event_stream),
            transition_period,
        }
    }
}

impl<EventStream> UninitializedEpochEventStream<EventStream>
where
    EventStream: futures::Stream + Unpin,
{
    /// Initializes a [`EpochEventStream`] by consuming the first [`Epoch`]
    /// from the underlying stream.
    ///
    /// It returns the first [`Epoch`] and the initialized
    /// [`EpochEventStream`], awaiting the first epoch for as long as
    /// necessary.
    /// It returns an error only if the underlying stream closes before yielding
    /// an epoch.
    pub async fn await_first_ready(
        self,
    ) -> Result<(EventStream::Item, EpochEventStream<EventStream>), FirstReadyStreamError> {
        let (first_epoch, remaining_stream) = self.stream.first().await?;
        Ok((
            first_epoch,
            EpochEventStream::new(remaining_stream, self.transition_period),
        ))
    }
}

#[derive(Clone, Debug)]
pub enum EpochEvent<Event> {
    NewEpoch(Event),
    TransitionPeriodExpired,
}

/// A stream that alternates between yielding [`EpochEvent::NewEpoch`]
/// and [`EpochEvent::TransitionPeriodExpired`].
///
/// It wraps a stream of [`Epoch`]s and yields a [`EpochEvent::NewEpoch`]
/// as soon as a new [`Epoch`] is available from the inner stream.
/// Then, it yields a [`EpochEvent::TransitionPeriodExpired`] after
/// the transition period has elapsed.
///
/// # Stream Timeline
/// ```text
/// event stream  : O--E-------O--E--------------O--E-------
/// epoch stream  : |----E1----|--------E2-------|----E3----
///
/// (O: NewEpoch, E: TransitionPeriodExpired, E*: Epochs)
/// ```
pub struct EpochEventStream<EventStream> {
    event_stream: EventStream,
    transition_period: Duration,
    transition_period_timer: Option<Pin<Box<Sleep>>>,
}

impl<EventStream> EpochEventStream<EventStream> {
    #[must_use]
    const fn new(event_stream: EventStream, transition_period: Duration) -> Self {
        Self {
            event_stream,
            transition_period,
            transition_period_timer: None,
        }
    }
}

impl<EventStream> futures::Stream for EpochEventStream<EventStream>
where
    EventStream: futures::Stream + Unpin,
{
    type Item = EpochEvent<EventStream::Item>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Check if a new epoch is available.
        match self.event_stream.poll_next_unpin(cx) {
            Poll::Ready(Some(epoch)) => {
                // Start the transition period timer, and yield the new epoch.
                // If the previous transition period timer has not been expired yet,
                // it will be overwritten.
                self.transition_period_timer = Some(Box::pin(sleep(self.transition_period)));
                return Poll::Ready(Some(EpochEvent::NewEpoch(epoch)));
            }
            Poll::Ready(None) => return Poll::Ready(None),
            Poll::Pending => {}
        }

        // Check if the transition period has expired.
        if let Some(timer) = &mut self.transition_period_timer
            && timer.as_mut().poll(cx).is_ready()
        {
            self.transition_period_timer = None;
            return Poll::Ready(Some(EpochEvent::TransitionPeriodExpired));
        }

        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt as _;
    use tokio::time::{Instant, interval};
    use tokio_stream::wrappers::IntervalStream;

    use super::*;

    #[tokio::test]
    async fn yield_two_events_alternately() {
        let epoch_duration = Duration::from_secs(1);
        let transition_period = Duration::from_millis(200);
        let time_tolerance = Duration::from_millis(100);

        let mut stream = EpochEventStream::new(
            Box::pin(IntervalStream::new(interval(epoch_duration))),
            transition_period,
        );

        // NewEpoch should be emitted immediately.
        let start_time = Instant::now();
        assert!(matches!(stream.next().await, Some(EpochEvent::NewEpoch(_))));
        let elapsed = start_time.elapsed();
        let tolerance = Duration::from_millis(50);
        assert!(elapsed <= tolerance, "elapsed:{elapsed:?}");

        // TransitionEnd should be emitted after transition_period.
        let start_time = Instant::now();
        assert!(matches!(
            stream.next().await,
            Some(EpochEvent::TransitionPeriodExpired)
        ));
        let elapsed = start_time.elapsed();
        assert!(
            elapsed.abs_diff(transition_period) <= time_tolerance,
            "elapsed:{elapsed:?}, expected:{transition_period:?}",
        );

        // NewEpoch should be emitted after epoch_duration - transition_period.
        let start_time = Instant::now();
        assert!(matches!(stream.next().await, Some(EpochEvent::NewEpoch(_))));
        let elapsed = start_time.elapsed();
        assert!(
            elapsed.abs_diff(epoch_duration.checked_sub(transition_period).unwrap())
                <= time_tolerance,
            "elapsed:{elapsed:?}, expected:{:?}",
            epoch_duration.checked_sub(transition_period).unwrap()
        );

        // TransitionEnd should be emitted after transition_period.
        let start_time = Instant::now();
        assert!(matches!(
            stream.next().await,
            Some(EpochEvent::TransitionPeriodExpired)
        ));
        let elapsed = start_time.elapsed();
        assert!(
            elapsed.abs_diff(transition_period) <= time_tolerance,
            "elapsed:{elapsed:?}, expected:{transition_period:?}",
        );
    }

    #[tokio::test]
    async fn transition_period_shorter_than_epoch() {
        let epoch_duration = Duration::from_millis(500);
        let transition_period = Duration::from_millis(600);
        let time_tolerance = Duration::from_millis(50);

        let mut stream = EpochEventStream::new(
            Box::pin(IntervalStream::new(interval(epoch_duration))),
            transition_period,
        );

        // NewEpoch should be emitted immediately.
        let start_time = Instant::now();
        assert!(matches!(stream.next().await, Some(EpochEvent::NewEpoch(_))));
        let elapsed = start_time.elapsed();
        assert!(elapsed <= time_tolerance, "elapsed:{elapsed:?}");

        // NewEpoch should be emitted again after epoch_duration.
        let start_time = Instant::now();
        assert!(matches!(stream.next().await, Some(EpochEvent::NewEpoch(_))));
        let elapsed = start_time.elapsed();
        assert!(
            elapsed.abs_diff(epoch_duration) <= time_tolerance,
            "elapsed:{elapsed:?}, expected:{epoch_duration:?}",
        );
    }

    #[tokio::test]
    async fn first_ready_stream_yields_first_item_immediately() {
        // Use an underlying stream that yields the first item nearly immediately.
        let stream = UninitializedFirstReadyStream::new(
            IntervalStream::new(interval(Duration::from_secs(1)))
                .enumerate()
                .map(|(i, _)| i),
        );

        let (first, mut stream) = stream.first().await.expect("first item should be yielded");
        assert_eq!(first, 0);
        // Next items are yielded normally.
        assert_eq!(stream.next().await, Some(1));
        assert_eq!(stream.next().await, Some(2));
    }
}
