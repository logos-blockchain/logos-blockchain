use core::time::Duration;
use std::sync::OnceLock;

use lb_core::mantle::transactions::genesis_tx::ChainId;
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

/// The chain ID of the deployment this process was started with.
///
/// The chain ID is fixed for the lifetime of a node: it is baked into the
/// deployment settings (see [`SERIALIZED_DEPLOYMENT`]) or into whichever
/// deployment file overrides them, and is never renegotiated at runtime.
/// [`run_node_from_config`](crate::run_node_from_config) records it here at
/// startup so read-only callers — the HTTP API and the C bindings — can answer
/// with it directly instead of relaying to the chain service for a value that
/// cannot change.
static CHAIN_ID: OnceLock<ChainId> = OnceLock::new();

/// Records the chain ID of the deployment this process runs.
///
/// Called once from [`run_node_from_config`](crate::run_node_from_config).
/// Later calls are ignored, so a second node started in the same process keeps
/// reporting the first one's chain ID; nothing in the node starts two.
pub(crate) fn record_chain_id(chain_id: ChainId) {
    drop(CHAIN_ID.set(chain_id));
}

/// The chain ID this process was started on, or `None` before the node has
/// been built from its configuration.
#[must_use]
pub fn chain_id() -> Option<&'static ChainId> {
    CHAIN_ID.get()
}

impl DeploymentSettings {
    /// The chain this deployment targets, read off the genesis inscription.
    #[must_use]
    pub fn chain_id(&self) -> ChainId {
        self.cryptarchia.chain_id()
    }

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
