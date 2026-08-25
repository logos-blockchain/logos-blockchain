use core::num::{NonZero, NonZeroU32, NonZeroU64};
use std::collections::HashMap;

use lb_chain_service::Epoch;
use lb_core::{
    block::genesis::GenesisBlock,
    sdp::{InactivityPeriod, MinStake, ServiceType},
};
use lb_cryptarchia_engine::{
    Config as ConsensusConfig, average_slots_for_blocks, base_period_length, time::epoch_length,
};
use lb_groth16::ModulusShift;
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_utils::math::{NonNegativeF64, NonNegativeRatio};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Settings {
    pub epoch_config: EpochConfig,
    pub security_param: NonZeroU32,
    pub slot_activation_coeff: NonNegativeRatio,
    pub learning_rate: NonNegativeF64,
    /// `W`, the uncle reference window in expected block-intervals.
    pub uncle_reference_window_in_block: NonZeroU32,
    pub sdp_config: SdpConfig,
    pub gossipsub_protocol: String,
    pub genesis_block: GenesisBlock,
    #[serde(default)]
    pub faucet_pk: Option<ZkPublicKey>,
    pub pow_config: PoWConfig,
}

impl Settings {
    #[must_use]
    pub const fn slots_per_epoch(&self) -> u64 {
        epoch_length(
            self.epoch_config.epoch_stake_distribution_stabilization,
            self.epoch_config.epoch_period_nonce_buffer,
            self.epoch_config.epoch_period_nonce_stabilization,
            base_period_length(self.security_param, self.slot_activation_coeff),
        )
    }

    #[must_use]
    pub const fn average_slots_per_block(&self) -> u64 {
        average_slots_for_blocks(
            NonZero::<u32>::new(1).expect("must be non-zero"),
            self.slot_activation_coeff,
        )
        .get()
    }

    #[must_use]
    pub fn consensus_config(&self) -> ConsensusConfig {
        ConsensusConfig::new(
            self.security_param,
            self.slot_activation_coeff,
            self.learning_rate,
            self.uncle_reference_window_in_block,
        )
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochConfig {
    // The stake distribution is always taken at the beginning of the previous epoch.
    // This parameters controls how many slots to wait for it to be stabilized
    // The value is computed as epoch_stake_distribution_stabilization * int(floor(k / f))
    pub epoch_stake_distribution_stabilization: NonZero<u8>,
    // This parameter controls how many slots we wait after the stake distribution
    // snapshot has stabilized to take the nonce snapshot.
    pub epoch_period_nonce_buffer: NonZero<u8>,
    // This parameter controls how many slots we wait for the nonce snapshot to be considered
    // stabilized
    pub epoch_period_nonce_stabilization: NonZero<u8>,
}

// The same as `lb_ledger::mantle::sdp::Config`, minus the
// `service_rewards_params` values, which are taken from the Blend deployment
// config instead.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SdpConfig {
    pub service_params: HashMap<ServiceType, ServiceParameters>,
    pub min_stake: MinStake,
}

// The same as `lb_core::sdp::ServiceParameters`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ServiceParameters {
    pub inactivity_period: InactivityPeriod,
    pub epoch: Epoch,
}

// The same as `lb_ledger::config::PoWConfig`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PoWConfig {
    pub blend: BlendPoWConfig,
    pub reward: RewardPoWConfig,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BlendPoWConfig {
    pub base_difficulty: ModulusShift,
    pub target_transactions_per_block: NonZeroU64,
    pub max_step: NonZeroU64,
    pub damping_num: NonZeroU32,
    pub damping_den_offset: u32,
}

// The reward parameters are used verbatim, so the ledger type is reused
// directly: deserializing the deployment config runs its invariant checks
// (`RewardPoWConfig::validate`), rejecting an invalid config at load time.
pub use lb_ledger::config::RewardPoWConfig;
