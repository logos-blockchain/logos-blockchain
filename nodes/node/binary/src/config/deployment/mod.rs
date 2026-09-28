use core::time::Duration;

use lb_core::{
    block::genesis::GenesisBlock,
    era::{EraDigest, ForkDigest},
    header::HeaderId,
    mantle::{
        traits::GenesisTx as _,
        transactions::genesis_tx::{ChainId, GenesisTime},
    },
};
use lb_ledger::mantle::sdp::rewards::blend::RewardsParameters;
use lb_utils::yaml::{OnUnknownKeys, deserialize_value_from_reader};
use serde::{Deserialize, Serialize};

use crate::config::network::deployment::Settings as NetworkDeploymentSettings;

mod era;
pub use era::{EraParameters, EraSchedule, EraScheduleError};

pub const SERIALIZED_DEPLOYMENT: &[u8] = include_bytes!("settings.yaml");

/// Everything that defines a chain: the parameters of each of its eras, and
/// the genesis block they start from.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DeploymentSettings {
    pub eras: EraSchedule,
    // TODO: These will be removed from the deployment settings and derived from the era
    // definitions instead, per era. To be done in a follow-up PR.
    pub network: NetworkDeploymentSettings,
    pub genesis_block: GenesisBlock,
}

impl DeploymentSettings {
    /// The chain this deployment targets, read off the genesis inscription.
    #[must_use]
    pub fn chain_id(&self) -> ChainId {
        self.genesis_block
            .genesis_tx()
            .cryptarchia_parameter()
            .chain_id
    }

    /// When this deployment's chain starts, read off the genesis inscription.
    #[must_use]
    pub fn genesis_time(&self) -> GenesisTime {
        self.genesis_block
            .genesis_tx()
            .cryptarchia_parameter()
            .genesis_time
    }

    /// The ID of the genesis block this deployment's chain starts from.
    #[must_use]
    pub fn genesis_id(&self) -> HeaderId {
        self.genesis_block.header().id()
    }

    /// The digest of the fork this deployment defines: of its genesis block,
    /// of its chain ID and of every era of its schedule, in activation order.
    /// With a single era supported, it names the fork a node follows from
    /// genesis on.
    #[must_use]
    pub fn fork_digest(&self) -> ForkDigest {
        let era_digests = self
            .eras
            .iter()
            .map(|(first_epoch, parameters)| EraDigest::compute(first_epoch, parameters));
        ForkDigest::compute(self.genesis_id(), &self.chain_id(), era_digests)
    }

    #[must_use]
    pub const fn genesis_era_parameters(&self) -> &EraParameters {
        self.eras.genesis_era_parameters()
    }

    pub const fn genesis_era_parameters_mut(&mut self) -> &mut EraParameters {
        self.eras.genesis_era_parameters_mut()
    }

    #[must_use]
    pub const fn genesis_blend_round_duration(&self) -> Duration {
        self.genesis_era_parameters().blend_round_duration()
    }

    #[must_use]
    pub fn genesis_blend_reward_params(&self) -> RewardsParameters {
        self.genesis_era_parameters().blend_reward_params()
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
    fn the_fork_digest_survives_a_round_trip_through_yaml() {
        // Not pinned to a value: the default deployment changes at every
        // genesis ceremony. What must hold is that the digests depend only on
        // the settings, not on how they were loaded.
        let settings = DeploymentSettings::default();
        let recovered: DeploymentSettings =
            serde_yaml::from_str(&serde_yaml::to_string(&settings).unwrap()).unwrap();
        assert_eq!(recovered.fork_digest(), settings.fork_digest());
    }

    #[test]
    fn the_fork_digest_commits_to_the_era_parameters() {
        let settings = DeploymentSettings::default();
        let mut changed = settings.clone();
        changed
            .genesis_era_parameters_mut()
            .cryptarchia
            .pow_config
            .reward
            .slot_window = changed
            .genesis_era_parameters()
            .cryptarchia
            .pow_config
            .reward
            .slot_window
            .checked_add(1)
            .unwrap();
        assert_ne!(changed.fork_digest(), settings.fork_digest());
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
        let cryptarchia = &settings.genesis_era_parameters().cryptarchia;
        let reward = &cryptarchia.pow_config.reward;
        let denominator = u128::from(reward.rate_den.get())
            * u128::from(reward.target_claim_per_block.get())
            * u128::from(cryptarchia.expected_blocks_per_epoch().get());
        assert_eq!(
            u128::from(reward.epoch_reward_genesis),
            u128::from(reward.reward_pool_genesis) * u128::from(reward.rate_num) / denominator,
        );
    }
}
