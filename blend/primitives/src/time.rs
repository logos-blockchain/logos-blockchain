use core::{
    fmt::{self, Display, Formatter},
    num::{NonZeroU64, NonZeroU128},
    task::Context,
    time::Duration,
};

use futures::Stream;
use tokio::time::{Instant, Interval, MissedTickBehavior, interval_at};

/// A round of the Blend clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Round(u128);

impl Round {
    #[must_use]
    pub const fn inner(&self) -> u128 {
        self.0
    }

    /// The number of whole rounds elapsed from `earlier` to `self`.
    #[must_use]
    pub const fn rounds_since(self, earlier: Self) -> u128 {
        self.0.saturating_sub(earlier.0)
    }
}

impl From<u128> for Round {
    fn from(value: u128) -> Self {
        Self(value)
    }
}

impl From<Round> for u128 {
    fn from(round: Round) -> Self {
        round.0
    }
}

impl Display for Round {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A number of rounds, for the windows and deadlines the protocol defines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RoundCount(NonZeroU128);

impl RoundCount {
    #[must_use]
    pub const fn new(rounds: NonZeroU128) -> Self {
        Self(rounds)
    }

    #[must_use]
    pub const fn get(self) -> u128 {
        self.0.get()
    }
}

impl From<NonZeroU128> for RoundCount {
    fn from(rounds: NonZeroU128) -> Self {
        Self::new(rounds)
    }
}

pub type RoundStream = Box<dyn Stream<Item = Round> + Send + Unpin>;

/// A round clock driven by a [`tokio::time::Interval`].
pub struct RoundClock {
    // We need a non-zero duration to be able to calculate the current round given a timestamp,
    // which involves dividing by the round duration.
    round_duration_in_seconds: NonZeroU64,
    start_time: Instant,
    interval: Interval,
}

impl RoundClock {
    /// Starts a clock whose round `0` begins now.
    #[must_use]
    pub fn new(round_duration_in_seconds: NonZeroU64) -> Self {
        Self::starting_at(Instant::now(), round_duration_in_seconds)
    }

    /// Starts a clock whose round `0` begins at `origin`.
    #[must_use]
    pub fn starting_at(start_time: Instant, round_duration_in_seconds: NonZeroU64) -> Self {
        let round_duration = Duration::from_secs(round_duration_in_seconds.get());
        let mut interval = interval_at(
            start_time
                .checked_add(round_duration)
                .expect("Overflow when computing clock start time."),
            round_duration,
        );
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        Self {
            round_duration_in_seconds,
            start_time,
            interval,
        }
    }

    /// The round that now falls in.
    #[must_use]
    pub fn current_round(&self) -> Round {
        let elapsed = Instant::now().saturating_duration_since(self.start_time);
        Round(u128::from(
            elapsed.as_secs() / self.round_duration_in_seconds.get(),
        ))
    }

    /// The round that now falls in, registering `cx` to be woken at the next
    /// round boundary.
    ///
    /// Callers should poll this before anything else they do, since for a task
    /// that is otherwise idle it is the only thing keeping it scheduled.
    pub fn poll_current(&mut self, cx: &mut Context<'_>) -> Round {
        // Drain the tick so the waker is registered for the next boundary. With
        // `Skip` there is at most one tick pending, so this loop runs at most
        // twice.
        while self.interval.poll_tick(cx).is_ready() {}
        self.current_round()
    }
}

#[cfg(test)]
mod tests {
    use core::{num::NonZeroU128, time::Duration};

    use tokio::time::{Instant, advance, pause};

    use crate::time::{Round, RoundClock, RoundCount};

    const ROUND_DURATION: Duration = Duration::from_secs(1);

    #[test]
    fn rounds_since_saturates_rather_than_wrapping() {
        assert_eq!(Round::from(2).rounds_since(Round::from(5)), 0);
        assert_eq!(Round::from(5).rounds_since(Round::from(2)), 3);
    }

    #[test]
    fn a_round_count_is_measured_in_the_same_unit_as_a_round_difference() {
        let window = RoundCount::new(NonZeroU128::new(30).unwrap());
        assert!(Round::from(29).rounds_since(Round::from(0)) < window.get());
        assert!(Round::from(30).rounds_since(Round::from(0)) >= window.get());
    }

    #[tokio::test]
    async fn round_advances_with_elapsed_time() {
        pause();
        let clock = RoundClock::new(ROUND_DURATION.as_secs().try_into().unwrap());
        let start = clock.current_round();

        advance(ROUND_DURATION).await;
        assert_eq!(clock.current_round().rounds_since(start), 1);

        advance(ROUND_DURATION * 4).await;
        assert_eq!(clock.current_round().rounds_since(start), 5);
    }

    #[tokio::test]
    async fn rounds_are_counted_from_elapsed_time_not_from_ticks() {
        pause();
        let clock = RoundClock::new(ROUND_DURATION.as_secs().try_into().unwrap());
        let start = clock.current_round();

        // Nothing polls the clock while ten rounds pass. A clock that counted
        // ticks would report one round; this one reports ten.
        advance(ROUND_DURATION * 10).await;

        assert_eq!(clock.current_round().rounds_since(start), 10);
    }

    #[tokio::test]
    async fn a_shared_origin_gives_two_clocks_the_same_rounds() {
        pause();
        let origin = Instant::now();
        let first = RoundClock::starting_at(origin, ROUND_DURATION.as_secs().try_into().unwrap());
        advance(ROUND_DURATION / 3).await;
        let second = RoundClock::starting_at(origin, ROUND_DURATION.as_secs().try_into().unwrap());

        advance(ROUND_DURATION * 3).await;

        assert_eq!(first.current_round(), second.current_round());
    }
}
