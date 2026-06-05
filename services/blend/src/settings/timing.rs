use core::{num::NonZeroU64, time::Duration};

use serde::{Deserialize, Serialize};

#[serde_with::serde_as]
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TimingSettings {
    /// `S`: length of an epoch in terms of rounds.
    pub rounds_per_epoch: NonZeroU64,
    #[serde_as(
        as = "lb_utils::bounded_duration::MinimalBoundedDuration<1, lb_utils::bounded_duration::SECOND>"
    )]
    /// Duration of a round.
    pub round_duration: Duration,
    pub rounds_per_observation_window: NonZeroU64,
    pub epoch_transition_period: Duration,
}
