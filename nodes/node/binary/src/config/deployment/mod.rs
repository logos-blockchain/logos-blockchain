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

    #[test]
    fn genesis_epoch_reward_matches_the_payout_rate() {
        // `epoch_reward_genesis` is not free: it must be the `sigma_e` the
        // first epoch boundary would compute for the genesis pool, or genesis
        // and steady state disagree. That is
        // `W0 * rate_num / (rate_den * target_claim_per_block * N_b)`, and
        // `N_b` follows from the consensus schedule — so changing
        // `security_param` or `slot_activation_coeff` moves this value too.
        let settings = DeploymentSettings::default();
        let reward = &settings.cryptarchia.pow_config.reward;
        let denominator = u128::from(reward.rate_den.get())
            * u128::from(reward.target_claim_per_block.get())
            * u128::from(settings.cryptarchia.expected_blocks_per_epoch().get());
        assert_eq!(
            u128::from(reward.epoch_reward_genesis),
            u128::from(reward.reward_pool_genesis) * u128::from(reward.rate_num) / denominator,
        );
    }
}
