use core::time::Duration;

use lb_ledger::mantle::sdp::rewards::blend::RewardsParameters;
use serde::{Deserialize, Serialize};

pub mod blend;
pub mod cryptarchia;
pub mod time;

use self::{
    blend::Settings as BlendDeploymentSettings,
    cryptarchia::Settings as CryptarchiaDeploymentSettings,
    time::Settings as TimeDeploymentSettings,
};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Parameters {
    pub blend: BlendDeploymentSettings,
    pub cryptarchia: CryptarchiaDeploymentSettings,
    pub time: TimeDeploymentSettings,
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
