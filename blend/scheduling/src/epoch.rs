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
///
/// `TransitionPeriod` says how long the transition into each epoch lasts, from
/// the epoch's event.
pub struct UninitializedEpochEventStream<EventStream, TransitionPeriod> {
    stream: UninitializedFirstReadyStream<EventStream>,
    transition_period: TransitionPeriod,
}

impl<EventStream, TransitionPeriod> UninitializedEpochEventStream<EventStream, TransitionPeriod> {
    #[must_use]
    pub const fn new(event_stream: EventStream, transition_period: TransitionPeriod) -> Self {
        Self {
            stream: UninitializedFirstReadyStream::new(event_stream),
            transition_period,
        }
    }
}

impl<EventStream, TransitionPeriod> UninitializedEpochEventStream<EventStream, TransitionPeriod>
where
    EventStream: futures::Stream + Unpin,
    TransitionPeriod: Fn(&EventStream::Item) -> Duration,
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
    ) -> Result<
        (
            EventStream::Item,
            EpochEventStream<EventStream, TransitionPeriod>,
        ),
        FirstReadyStreamError,
    > {
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
/// the transition period of that epoch has elapsed.
///
/// # Stream Timeline
/// ```text
/// event stream  : O--E-------O--E--------------O--E-------
/// epoch stream  : |----E1----|--------E2-------|----E3----
///
/// (O: NewEpoch, E: TransitionPeriodExpired, E*: Epochs)
/// ```
pub struct EpochEventStream<EventStream, TransitionPeriod> {
    event_stream: EventStream,
    transition_period: TransitionPeriod,
    transition_period_timer: Option<Pin<Box<Sleep>>>,
}

impl<EventStream, TransitionPeriod> EpochEventStream<EventStream, TransitionPeriod> {
    #[must_use]
    const fn new(event_stream: EventStream, transition_period: TransitionPeriod) -> Self {
        Self {
            event_stream,
            transition_period,
            transition_period_timer: None,
        }
    }
}

impl<EventStream, TransitionPeriod> futures::Stream
    for EpochEventStream<EventStream, TransitionPeriod>
where
    EventStream: futures::Stream + Unpin,
    TransitionPeriod: Fn(&EventStream::Item) -> Duration + Unpin,
{
    type Item = EpochEvent<EventStream::Item>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Check if a new epoch is available.
        match self.event_stream.poll_next_unpin(cx) {
            Poll::Ready(Some(epoch)) => {
                // Start the transition period timer, and yield the new epoch.
                // If the previous transition period timer has not been expired yet,
                // it will be overwritten.
                let transition_period = (self.transition_period)(&epoch);
                self.transition_period_timer = Some(Box::pin(sleep(transition_period)));
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
            move |_: &_| transition_period,
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
            move |_: &_| transition_period,
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
    async fn each_epoch_has_its_own_transition_period() {
        let time_tolerance = Duration::from_millis(50);
        // The first epoch's transition lasts 100 ms, the second's 300 ms.
        let mut stream = EpochEventStream::new(
            Box::pin(
                IntervalStream::new(interval(Duration::from_secs(1)))
                    .enumerate()
                    .map(|(epoch, _)| epoch),
            ),
            |epoch: &usize| Duration::from_millis(if *epoch == 0 { 100 } else { 300 }),
        );

        for expected_transition in [100, 300].map(Duration::from_millis) {
            assert!(matches!(stream.next().await, Some(EpochEvent::NewEpoch(_))));
            let start_time = Instant::now();
            assert!(matches!(
                stream.next().await,
                Some(EpochEvent::TransitionPeriodExpired)
            ));
            let elapsed = start_time.elapsed();
            assert!(
                elapsed.abs_diff(expected_transition) <= time_tolerance,
                "elapsed:{elapsed:?}, expected:{expected_transition:?}",
            );
        }
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
