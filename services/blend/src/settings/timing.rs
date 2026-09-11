use core::{
    num::{NonZeroU64, NonZeroU128},
    time::Duration,
};

use serde::{Deserialize, Serialize};

#[serde_with::serde_as]
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TimingSettings {
    /// `S`: length of an epoch in terms of rounds.
    pub rounds_per_epoch: NonZeroU64,
    pub round_duration_in_seconds: NonZeroU64,
    pub rounds_per_observation_window: NonZeroU128,
    pub epoch_transition_period: Duration,
}
