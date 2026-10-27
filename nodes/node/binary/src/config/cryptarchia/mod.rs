use std::sync::Arc;

use lb_chain_network_service::network::adapters::libp2p::{
    LibP2pAdapterEraSettings, LibP2pAdapterSettings,
};
use lb_core::{
    block::genesis::GenesisBlock, mantle::traits::GenesisTx as _, sdp::ServiceParameters,
};
use lb_cryptarchia_engine::{EpochConfig, EraSchedule};
use lb_ledger::mantle::sdp::ServiceRewardsParameters;
use lb_libp2p::{PeerId, protocol_name::ProtocolScope};
use lb_services_utils::overwatch::RecoveryData;

use crate::config::{
    cryptarchia::serde::Config,
    deployment::{EraDefinition, era::ruleset::EraRuleset},
};

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    /// The settings of the chain network: the user's, and in each era of `eras`
    /// the era's block proposal topic and active slot coefficient.
    #[must_use]
    pub fn into_chain_network_settings(
        &self,
        eras: &EraSchedule<EraDefinition>,
    ) -> lb_chain_network_service::ChainNetworkSettings<PeerId, LibP2pAdapterSettings> {
        let network = self.user.network.clone();
        lb_chain_network_service::ChainNetworkSettings {
            bootstrap: lb_chain_network_service::BootstrapConfig {
                ibd: lb_chain_network_service::IbdConfig {
                    peers: network.bootstrap.ibd.peers,
                    tips_fetch_max_attempts: network.bootstrap.ibd.tips_fetch_max_attempts,
                    tips_fetch_min_delay: network.bootstrap.ibd.tips_fetch_min_delay,
                    tips_fetch_max_delay: network.bootstrap.ibd.tips_fetch_max_delay,
                    round_delay: network.bootstrap.ibd.round_delay,
                },
            },
            network: LibP2pAdapterSettings {
                max_connected_peers_to_try_download: network
                    .network
                    .max_connected_peers_to_try_download,
                max_discovered_peers_to_try_download: network
                    .network
                    .max_discovered_peers_to_try_download,
                era_schedule: eras.map(|era| LibP2pAdapterEraSettings {
                    topic: ProtocolScope::Fork(era.parameters.fork_digest)
                        .to_string_with_name("cryptarchia"),
                }),
            },
            sync: lb_chain_network_service::SyncConfig {
                orphan: lb_chain_network_service::OrphanConfig {
                    max_orphan_cache_size: network.sync.orphan.max_orphan_cache_size,
                    max_rejected_cache_size: network.sync.orphan.max_rejected_cache_size,
                },
                tip_poll: lb_chain_network_service::TipPollConfig {
                    enabled: network.sync.tip_poll.enabled,
                    lag_threshold_blocks: network.sync.tip_poll.lag_threshold_blocks,
                    max_peers_to_sample: network.sync.tip_poll.max_peers_to_sample,
                },
            },
            seen_items_subscription_channel_size: network
                .seen_items_subscription_channel_size
                .try_into()
                .unwrap(),
            era_schedule: eras.map(|era| {
                let EraRuleset::V1(parameters) = &era.parameters.ruleset;
                lb_chain_network_service::ChainNetworkEraSettings {
                    slot_activation_coeff: parameters.cryptarchia.slot_activation_coeff,
                }
            }),
        }
    }

    /// The settings of the chain service and the leader: the user's, the
    /// ledger config of every era of `eras`, which both share, and for the
    /// leader the consensus-excluded keys of `genesis_block`.
    #[must_use]
    pub fn into_cryptarchia_services_settings(
        self,
        eras: &EraSchedule<EraDefinition>,
        genesis_block: &GenesisBlock,
        recovery_data: &RecoveryData,
    ) -> (
        lb_chain_service::CryptarchiaSettings,
        lb_chain_leader_service::LeaderSettings,
    ) {
        let user = self.user;
        let era_schedule = Arc::new(eras.map(|era| ledger_config_for_era(era.parameters)));
        let chain_service_settings = lb_chain_service::CryptarchiaSettings {
            bootstrap: lb_chain_service::BootstrapConfig {
                force_bootstrap: user.service.bootstrap.force_bootstrap,
                prolonged_bootstrap_period: user.service.bootstrap.prolonged_bootstrap_period,
                offline_grace_period: lb_chain_service::OfflineGracePeriodConfig {
                    grace_period: user.service.bootstrap.offline_grace_period.grace_period,
                    state_recording_interval: user
                        .service
                        .bootstrap
                        .offline_grace_period
                        .state_recording_interval,
                },
            },
            era_schedule: Arc::clone(&era_schedule),
            recovery_data: recovery_data.clone(),
            starting_state: genesis_block.clone().into(),
            sync: lb_chain_service::SyncConfig {
                block_provider: lb_chain_service::BlockProviderConfig {
                    batch_size: user.service.sync.block_provider.batch_size,
                },
            },
        };
        let chain_leader_settings = lb_chain_leader_service::LeaderSettings {
            era_schedule,
            consensus_excluded_keys: genesis_block
                .genesis_tx()
                .genesis_parameters()
                .consensus_excluded_keys,
            wallet_config: lb_chain_leader_service::LeaderWalletConfig {
                funding_pk: user.leader.wallet.funding_pk,
                max_tx_fee: user.leader.wallet.max_tx_fee,
            },
        };
        (chain_service_settings, chain_leader_settings)
    }
}

/// The ledger config of an era.
fn ledger_config_for_era(era: &EraDefinition) -> lb_ledger::Config {
    let EraRuleset::V1(parameters) = &era.ruleset;
    let deployment = &parameters.cryptarchia;
    let blend_rewards_params = parameters.blend_reward_params();
    lb_ledger::Config {
        consensus_config: deployment.consensus_config(),
        epoch_config: EpochConfig {
            epoch_period_nonce_buffer: deployment.epoch_config.epoch_period_nonce_buffer,
            epoch_period_nonce_stabilization: deployment
                .epoch_config
                .epoch_period_nonce_stabilization,
            epoch_stake_distribution_stabilization: deployment
                .epoch_config
                .epoch_stake_distribution_stabilization,
        },
        sdp_config: lb_ledger::mantle::sdp::Config {
            min_stake: deployment.sdp_config.min_stake,
            service_params: Arc::new(
                deployment
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
                blend: blend_rewards_params,
            },
        },
        pow_config: lb_ledger::config::PoWConfig {
            blend: lb_ledger::config::BlendPoWConfig {
                base_difficulty: deployment.pow_config.blend.base_difficulty,
                damping_den_offset: deployment.pow_config.blend.damping_den_offset,
                damping_num: deployment.pow_config.blend.damping_num,
                max_step: deployment.pow_config.blend.max_step,
                target_transactions_per_block: deployment
                    .pow_config
                    .blend
                    .target_transactions_per_block,
            },
            // Reused verbatim: the deployment mirror already holds the
            // validated ledger type.
            reward: deployment.pow_config.reward.clone(),
        },
    }
}
