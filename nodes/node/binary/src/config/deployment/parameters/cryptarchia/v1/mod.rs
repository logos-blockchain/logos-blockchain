use core::num::{NonZero, NonZeroU32, NonZeroU64};
use std::sync::Arc;

use lb_binary_codec::canonical::BTreeMap;
use lb_core::sdp::{InactivityPeriod, MinStake, ServiceType};
use lb_cryptarchia_engine::{
    Config as ConsensusConfig, Epoch, average_slots_for_blocks, base_period_length,
    expected_blocks_per_epoch, time::epoch_length,
};
use lb_groth16::ModulusShift;
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_ledger::mantle::sdp::{ServiceRewardsParameters, rewards::blend::RewardsParameters};
use lb_utils::math::{NonNegativeF64, NonNegativeRatio};
use serde::{Deserialize, Serialize};

pub(crate) mod codec;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Settings {
    pub epoch_config: EpochConfig,
    pub security_param: NonZeroU32,
    pub slot_activation_coeff: NonNegativeRatio,
    pub learning_rate: NonNegativeF64,
    /// `W`, the uncle reference window in expected block-intervals.
    pub uncle_reference_window_in_block: NonZeroU32,
    pub sdp_config: SdpConfig,
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

    /// `N_b`: the number of blocks an epoch is expected to produce, derived
    /// from the schedule rather than configured. The `PoW` payout rate is
    /// denominated in it — see `lb_ledger::config::Config`.
    #[must_use]
    pub const fn expected_blocks_per_epoch(&self) -> NonZeroU64 {
        expected_blocks_per_epoch(self.slots_per_epoch(), self.slot_activation_coeff)
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

    /// The ledger's configuration in its version 1: the consensus, epoch, SDP
    /// and `PoW` parameters, with `blend_rewards`, the Blend rewards the era's
    /// Blend section implies.
    #[must_use]
    pub fn ledger_config(&self, blend_rewards: RewardsParameters) -> lb_ledger::config::v1::Config {
        let epoch_config = &self.epoch_config;
        let blend_pow_config = &self.pow_config.blend;
        lb_ledger::config::v1::Config {
            consensus_config: self.consensus_config(),
            epoch_config: lb_cryptarchia_engine::EpochConfig {
                epoch_period_nonce_buffer: epoch_config.epoch_period_nonce_buffer,
                epoch_period_nonce_stabilization: epoch_config.epoch_period_nonce_stabilization,
                epoch_stake_distribution_stabilization: epoch_config
                    .epoch_stake_distribution_stabilization,
            },
            faucet_pk: self.faucet_pk,
            sdp_config: lb_ledger::mantle::sdp::Config {
                min_stake: self.sdp_config.min_stake,
                service_params: Arc::new(
                    self.sdp_config
                        .service_params
                        .iter()
                        .map(|(service_type, service_params)| {
                            (
                                *service_type,
                                lb_core::sdp::ServiceParameters {
                                    inactivity_period: service_params.inactivity_period,
                                    epoch: service_params.epoch,
                                },
                            )
                        })
                        .collect(),
                ),
                service_rewards_params: ServiceRewardsParameters {
                    blend: blend_rewards,
                },
            },
            pow_config: lb_ledger::config::PoWConfig {
                blend: lb_ledger::config::BlendPoWConfig {
                    base_difficulty: blend_pow_config.base_difficulty,
                    damping_den_offset: blend_pow_config.damping_den_offset,
                    damping_num: blend_pow_config.damping_num,
                    max_step: blend_pow_config.max_step,
                    target_transactions_per_block: blend_pow_config.target_transactions_per_block,
                },
                // Reused verbatim: the parameters already hold the validated
                // ledger type.
                reward: self.pow_config.reward.clone(),
            },
        }
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
    /// Ordered by service type, which is the order the canonical encoding lists
    /// them in.
    pub service_params: BTreeMap<ServiceType, ServiceParameters>,
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
