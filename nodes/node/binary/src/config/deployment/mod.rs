use core::time::Duration;

use lb_ledger::mantle::sdp::rewards::blend::RewardsParameters;
use lb_utils::yaml::{OnUnknownKeys, deserialize_value_from_reader};
use serde::{Deserialize, Serialize};

use crate::config::{
    blend::deployment::Settings as BlendDeploymentSettings,
    cryptarchia::deployment::Settings as CryptarchiaDeploymentSettings,
    mempool::deployment::Settings as MempoolDeploymentSettings,
    network::deployment::Settings as NetworkDeploymentSettings,
    time::deployment::Settings as TimeDeploymentSettings,
};

pub const SERIALIZED_DEPLOYMENT: &[u8] = include_bytes!("settings.yaml");

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DeploymentSettings {
    pub blend: BlendDeploymentSettings,
    pub network: NetworkDeploymentSettings,
    pub cryptarchia: CryptarchiaDeploymentSettings,
    pub time: TimeDeploymentSettings,
    pub mempool: MempoolDeploymentSettings,
}

impl DeploymentSettings {
    #[must_use]
    pub const fn blend_round_duration(&self) -> Duration {
        self.blend.round_duration(&self.time.slot_duration)
    }

    #[must_use]
    pub fn blend_reward_params(&self) -> RewardsParameters {
        self.blend.rewards_params(&self.cryptarchia, &self.time)
    }
}

impl Default for DeploymentSettings {
    fn default() -> Self {
        deserialize_value_from_reader(SERIALIZED_DEPLOYMENT, OnUnknownKeys::Fail)
            .expect("Default deployment settings must be valid.")
    }
}

#[cfg(test)]
mod tests {
    use crate::config::DeploymentSettings;

    #[test]
    fn default_initialization() {
        drop(DeploymentSettings::default());
    }

    #[test]
    fn serialize_deserialize_yaml() {
        let settings = DeploymentSettings::default();
        let as_str = serde_yaml::to_string(&settings).unwrap();
        let _recovered: DeploymentSettings = serde_yaml::from_str(&as_str).unwrap();
    }
}
