use core::{
    mem,
    pin::Pin,
    task::{Context, Poll},
};
use std::num::{NonZeroU64, NonZeroU128};

use futures::{Stream, StreamExt as _};
use tracing::{debug, trace};

use crate::message_scheduler::round_info::Round;

const LOG_TARGET: &str = "blend::scheduling::delay";

/// A scheduler for delaying processed messages and batch them into release
/// rounds, as per the specification.
pub struct SessionProcessedMessageDelayer<RoundClock, Rng, ProcessedMessage> {
    /// The maximum delay, in terms of rounds, between two consecutive release
    /// rounds.
    maximum_release_delay_in_rounds: NonZeroU64,
    /// The next scheduled round to release the queued messages.
    next_release_round: Round,
    /// The random generator to select rounds for message releasing.
    rng: Rng,
    /// The clock ticking at the beginning of each new round.
    round_clock: RoundClock,
    /// The messages temporarily queued by the scheduler in between release
    /// rounds.
    unreleased_messages: Vec<ProcessedMessage>,
}

impl<RoundClock, Rng, ProcessedMessage>
    SessionProcessedMessageDelayer<RoundClock, Rng, ProcessedMessage>
where
    Rng: rand::Rng,
{
    /// Initialize the scheduler with the required details.
    pub fn new(
        Settings {
            maximum_release_delay_in_rounds,
        }: Settings,
        mut rng: Rng,
        round_clock: RoundClock,
    ) -> Self {
        debug!(target: LOG_TARGET, "Creating new release delayer with {maximum_release_delay_in_rounds} maximum delay (in rounds).");

        let next_release_round =
            schedule_next_release_round_offset(maximum_release_delay_in_rounds, &mut rng);
        Self {
            maximum_release_delay_in_rounds,
            next_release_round: NonZeroU128::from(next_release_round).get().into(),
            rng,
            round_clock,
            unreleased_messages: Vec::new(),
        }
    }
}

impl<RoundClock, Rng, ProcessedMessage>
    SessionProcessedMessageDelayer<RoundClock, Rng, ProcessedMessage>
{
    #[cfg(test)]
    pub const fn with_test_values(
        maximum_release_delay_in_rounds: NonZeroU64,
        next_release_round: Round,
        rng: Rng,
        round_clock: RoundClock,
        unreleased_messages: Vec<ProcessedMessage>,
    ) -> Self {
        Self {
            maximum_release_delay_in_rounds,
            next_release_round,
            rng,
            round_clock,
            unreleased_messages,
        }
    }

    #[cfg(any(test, feature = "unsafe-test-functions"))]
    pub fn unreleased_messages(&self) -> &[ProcessedMessage] {
        &self.unreleased_messages
    }

    /// Add a new message to the queue to be released at the next release round
    /// along with any other queued message.
    pub fn schedule_message(&mut self, message: ProcessedMessage) {
        self.unreleased_messages.push(message);
        debug!(target: LOG_TARGET, "New message scheduled. Pending message count: {}", self.unreleased_messages.len());
    }

    /// Get a reference to the stored rng.
    pub const fn rng(&self) -> &Rng {
        &self.rng
    }
}

fn calculate_next_release_round<Rng>(
    current_round: Round,
    maximum_release_delay_in_rounds: NonZeroU64,
    rng: &mut Rng,
) -> Round
where
    Rng: rand::Rng,
{
    let next_release_offset =
        schedule_next_release_round_offset(maximum_release_delay_in_rounds, rng);
    current_round
        .inner()
        .checked_add(next_release_offset.get().into())
        .expect("Overflow when scheduling next release round offset.")
        .into()
}

/// Calculates the random offset, in the range [1, `max`], from the current
/// round as the next release round.
fn schedule_next_release_round_offset<Rng>(
    maximum_release_delay_in_rounds: NonZeroU64,
    rng: &mut Rng,
) -> NonZeroU64
where
    Rng: rand::Rng,
{
    NonZeroU64::try_from(rng.gen_range(1..=maximum_release_delay_in_rounds.get()))
        .expect("Randomly generated offset should be strictly positive.")
}

impl<RoundClock, Rng, ProcessedMessage> Stream
    for SessionProcessedMessageDelayer<RoundClock, Rng, ProcessedMessage>
where
    RoundClock: Stream<Item = Round> + Unpin,
    Rng: rand::Rng + Unpin,
    ProcessedMessage: Unpin,
{
    type Item = Vec<ProcessedMessage>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let new_round = match self.round_clock.poll_next_unpin(cx) {
            Poll::Ready(Some(new_round)) => new_round,
            Poll::Ready(None) => return Poll::Ready(None),
            Poll::Pending => return Poll::Pending,
        };

        // If it's time to release messages...
        if new_round == self.next_release_round {
            // Reset the next release round.
            self.next_release_round = calculate_next_release_round(
                new_round,
                self.maximum_release_delay_in_rounds,
                &mut self.rng,
            );
            // Return the unreleased messages and clear the buffer.
            let messages = mem::take(&mut self.unreleased_messages);
            debug!(target: LOG_TARGET, "Round {new_round} is a release round.");
            return Poll::Ready(Some(messages));
        }
        trace!(target: LOG_TARGET, "Round {new_round} is not a release round.");
        // Awake to trigger a new round clock tick.
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

/// The settings to initialize the release delayer scheduler.
#[derive(Debug, Clone, Copy)]
pub struct Settings {
    /// The max number of rounds to wait between two release rounds, as per the
    /// specification.
    pub maximum_release_delay_in_rounds: NonZeroU64,
}

#[cfg(test)]
mod tests {
    use core::task::{Context, Poll};
    use std::num::NonZeroU64;

    use futures::{StreamExt as _, io::empty, task::noop_waker_ref};
    use lb_utils::blake_rng::BlakeRng;
    use rand::SeedableRng as _;
    use tokio_stream::iter;

    use crate::release_delayer::SessionProcessedMessageDelayer;

    #[test]
    fn poll_none_on_unscheduled_round() {
        let mut delayer = SessionProcessedMessageDelayer::<_, _, ()> {
            maximum_release_delay_in_rounds: NonZeroU64::new(3).expect("Non-zero usize"),
            next_release_round: 1u128.into(),
            rng: BlakeRng::from_entropy(),
            round_clock: iter([0u128]).map(Into::into),
            unreleased_messages: vec![],
        };
        let mut cx = Context::from_waker(noop_waker_ref());

        assert_eq!(delayer.poll_next_unpin(&mut cx), Poll::Pending);
        // Check that next release round has not changed.
        assert_eq!(delayer.next_release_round, 1u128.into());
    }

    #[test]
    fn poll_empty_on_scheduled_round_with_empty_queue() {
        let mut delayer = SessionProcessedMessageDelayer::<_, _, ()> {
            maximum_release_delay_in_rounds: NonZeroU64::new(3).expect("Non-zero usize"),
            next_release_round: 1u128.into(),
            rng: BlakeRng::from_entropy(),
            round_clock: iter([1u128]).map(Into::into),
            unreleased_messages: vec![],
        };
        let mut cx = Context::from_waker(noop_waker_ref());

        assert_eq!(delayer.poll_next_unpin(&mut cx), Poll::Ready(Some(vec![])));
        // Check that next release round has been updated with an offset in the
        // expected range [1, 3], so the new value must be in the range [2, 4].
        assert!((2..=4).contains(&delayer.next_release_round.inner()));
    }

    #[test]
    fn poll_non_empty_on_scheduled_round_with_non_empty_queue() {
        let mut delayer = SessionProcessedMessageDelayer::<_, _, ()> {
            maximum_release_delay_in_rounds: NonZeroU64::new(3).expect("Non-zero usize"),
            next_release_round: 1u128.into(),
            rng: BlakeRng::from_entropy(),
            round_clock: iter([1u128]).map(Into::into),
            unreleased_messages: vec![()],
        };
        let mut cx = Context::from_waker(noop_waker_ref());

        assert_eq!(
            delayer.poll_next_unpin(&mut cx),
            Poll::Ready(Some(vec![()]))
        );
        // Check that next release round has been updated with an offset in the
        // expected range [1, 3], so the new value must be in the range [2, 4].
        assert!((2..=4).contains(&delayer.next_release_round.inner()));
    }

    #[test]
    fn add_message_to_queue() {
        let mut delayer = SessionProcessedMessageDelayer::<_, _, bool> {
            maximum_release_delay_in_rounds: NonZeroU64::new(3).expect("Non-zero usize"),
            next_release_round: 1u128.into(),
            rng: BlakeRng::from_entropy(),
            round_clock: empty(),
            unreleased_messages: vec![],
        };
        delayer.schedule_message(true);
        // Check that the new message was added to the queue.
        assert_eq!(delayer.unreleased_messages, vec![true]);
    }
}
