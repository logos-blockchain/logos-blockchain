use core::time::Duration;

use lb_ledger::mantle::sdp::rewards::blend::RewardsParameters;
use serde::{Deserialize, Serialize};

pub mod blend;
pub mod cryptarchia;
pub mod time;

pub(super) mod codec;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Parameters {
    pub blend: blend::Settings,
    pub cryptarchia: cryptarchia::Settings,
    pub time: time::Settings,
}

impl Parameters {
    #[must_use]
    pub const fn blend_round_duration(&self) -> Duration {
        self.blend.round_duration(&self.time.slot_duration)
    }

    #[must_use]
    pub fn blend_reward_params(&self) -> RewardsParameters {
        self.blend.rewards_params(&self.cryptarchia, &self.time)
    }
}
