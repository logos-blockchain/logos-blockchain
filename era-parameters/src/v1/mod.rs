use core::time::Duration;
use std::sync::Arc;

use lb_core::sdp::ServiceParameters;
use lb_cryptarchia_engine::EpochConfig;
use lb_ledger::{
    config::{BlendPoWConfig, PoWConfig},
    mantle::sdp::{
        Config as SdpConfig, ServiceRewardsParameters, rewards::blend::RewardsParameters,
    },
};
use serde::{Deserialize, Serialize};

pub mod blend;
pub mod cryptarchia;
pub mod time;

pub(super) mod codec;

/// How long, in Blend rounds from the first slot of an era, the network keeps
/// accepting the identifiers of the era before it.
const ERA_TRANSITION_ROUNDS: u64 = 30;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Parameters {
    pub blend: blend::Settings,
    pub cryptarchia: cryptarchia::Settings,
    pub time: time::Settings,
}

impl Parameters {
    /// The era's transition period, in slots: [`ERA_TRANSITION_ROUNDS`]
    /// rounds, each lasting a slot ([`blend::Settings::round_duration`]).
    #[must_use]
    pub const fn transition_slots(&self) -> u64 {
        ERA_TRANSITION_ROUNDS
    }

    #[must_use]
    pub const fn blend_round_duration(&self) -> Duration {
        self.blend.round_duration(&self.time.slot_duration)
    }

    #[must_use]
    pub fn blend_reward_params(&self) -> RewardsParameters {
        self.blend.rewards_params(&self.cryptarchia, &self.time)
    }

    /// The ledger's configuration: the consensus, epoch, SDP and `PoW`
    /// parameters, with the Blend rewards they imply.
    #[must_use]
    pub fn ledger_config(&self) -> lb_ledger::Config {
        let cryptarchia = &self.cryptarchia;
        let epoch_config = &cryptarchia.epoch_config;
        let blend_pow_config = &cryptarchia.pow_config.blend;
        lb_ledger::Config {
            consensus_config: cryptarchia.consensus_config(),
            epoch_config: EpochConfig {
                epoch_period_nonce_buffer: epoch_config.epoch_period_nonce_buffer,
                epoch_period_nonce_stabilization: epoch_config.epoch_period_nonce_stabilization,
                epoch_stake_distribution_stabilization: epoch_config
                    .epoch_stake_distribution_stabilization,
            },
            faucet_pk: cryptarchia.faucet_pk,
            sdp_config: SdpConfig {
                min_stake: cryptarchia.sdp_config.min_stake,
                service_params: Arc::new(
                    cryptarchia
                        .sdp_config
                        .service_params
                        .iter()
                        .map(|(service_type, service_params)| {
                            (
                                *service_type,
                                ServiceParameters {
                                    inactivity_period: service_params.inactivity_period,
                                    epoch: service_params.epoch,
                                },
                            )
                        })
                        .collect(),
                ),
                service_rewards_params: ServiceRewardsParameters {
                    blend: self.blend_reward_params(),
                },
            },
            pow_config: PoWConfig {
                blend: BlendPoWConfig {
                    base_difficulty: blend_pow_config.base_difficulty,
                    damping_den_offset: blend_pow_config.damping_den_offset,
                    damping_num: blend_pow_config.damping_num,
                    max_step: blend_pow_config.max_step,
                    target_transactions_per_block: blend_pow_config.target_transactions_per_block,
                },
                // Reused verbatim: the parameters already hold the validated
                // ledger type.
                reward: cryptarchia.pow_config.reward.clone(),
            },
        }
    }
}
