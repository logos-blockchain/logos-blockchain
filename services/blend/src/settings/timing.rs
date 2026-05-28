use core::{num::NonZeroU64, time::Duration};

use serde::{Deserialize, Serialize};

#[serde_with::serde_as]
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TimingSettings {
    /// `S`: length of an epoch in terms of expected rounds (on average).
    pub rounds_per_epoch: NonZeroU64,
    /// `|I|`: length of an interval in terms of rounds.
    pub rounds_per_interval: NonZeroU64,
    #[serde_as(
        as = "lb_utils::bounded_duration::MinimalBoundedDuration<1, lb_utils::bounded_duration::SECOND>"
    )]
    /// Duration of a round.
    pub round_duration: Duration,
    pub rounds_per_observation_window: NonZeroU64,
    /// Epoch transition period in rounds.
    pub rounds_per_epoch_transition_period: NonZeroU64,
    pub epoch_transition_period: Duration,
}

impl TimingSettings {
    #[must_use]
    pub fn intervals_per_epoch(&self) -> NonZeroU64 {
        NonZeroU64::try_from(self.rounds_per_epoch.get() / self.rounds_per_interval.get()).expect("Obtained `0` when calculating the number of intervals per epoch, which is not allowed.")
    }
}
