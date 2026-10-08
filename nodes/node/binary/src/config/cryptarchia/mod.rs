use std::sync::Arc;

use lb_chain_network_service::network::adapters::libp2p::LibP2pAdapterSettings;
use lb_core::{block::genesis::GenesisBlock, sdp::ServiceParameters};
use lb_cryptarchia_engine::{EpochConfig, era::EraSchedule};
use lb_ledger::mantle::sdp::ServiceRewardsParameters;
use lb_libp2p::PeerId;
use lb_services_utils::overwatch::RecoveryData;

use crate::config::{
    cryptarchia::serde::Config,
    deployment::{EraDefinition, ProtocolScope, era::parameters::EraParameters},
};

pub mod serde;

/// The three settings a cryptarchia deployment produces: the chain service's,
/// the chain network's and the leader's.
type CryptarchiaServicesSettings = (
    lb_chain_service::CryptarchiaSettings,
    lb_chain_network_service::ChainNetworkSettings<PeerId, LibP2pAdapterSettings>,
    lb_chain_leader_service::LeaderSettings,
);

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    /// The settings of the chain services in every era of `eras`.
    #[must_use]
    #[expect(
        clippy::too_many_lines,
        reason = "Conversion. Useful to have in a single place."
    )]
    pub fn into_cryptarchia_services_era_schedule(
        self,
        eras: &EraSchedule<EraDefinition>,
        genesis_block: &GenesisBlock,
        recovery_data: &RecoveryData,
    ) -> EraSchedule<CryptarchiaServicesSettings> {
        eras.map(|era| {
            let user = self.user.clone();
            let definition = &era.entry.parameters;
            let EraParameters::V1(parameters) = &definition.parameters;
            let deployment = &parameters.cryptarchia;
            let blend_rewards_params = parameters.blend_reward_params();
            let ledger_config = lb_ledger::Config {
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
                faucet_pk: deployment.faucet_pk,
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
            };

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
                config: ledger_config.clone(),
                // TODO: This will go once we update the cryptarchia service group to support era
                // schedules.
                recovery_data: recovery_data.clone(),
                starting_state: genesis_block.clone().into(),
                sync: lb_chain_service::SyncConfig {
                    block_provider: lb_chain_service::BlockProviderConfig {
                        batch_size: user.service.sync.block_provider.batch_size,
                    },
                },
            };
            let chain_network_settings = lb_chain_network_service::ChainNetworkSettings {
                bootstrap: lb_chain_network_service::BootstrapConfig {
                    ibd: lb_chain_network_service::IbdConfig {
                        peers: user.network.bootstrap.ibd.peers,
                        tips_fetch_max_attempts: user.network.bootstrap.ibd.tips_fetch_max_attempts,
                        tips_fetch_min_delay: user.network.bootstrap.ibd.tips_fetch_min_delay,
                        tips_fetch_max_delay: user.network.bootstrap.ibd.tips_fetch_max_delay,
                        round_delay: user.network.bootstrap.ibd.round_delay,
                    },
                },
                network: LibP2pAdapterSettings {
                    topic: ProtocolScope::Fork(definition.fork_digest)
                        .to_string_with_name("cryptarchia"),
                    max_connected_peers_to_try_download: user
                        .network
                        .network
                        .max_connected_peers_to_try_download,
                    max_discovered_peers_to_try_download: user
                        .network
                        .network
                        .max_discovered_peers_to_try_download,
                },
                sync: lb_chain_network_service::SyncConfig {
                    orphan: lb_chain_network_service::OrphanConfig {
                        max_orphan_cache_size: user.network.sync.orphan.max_orphan_cache_size,
                        max_rejected_cache_size: user.network.sync.orphan.max_rejected_cache_size,
                    },
                    tip_poll: lb_chain_network_service::TipPollConfig {
                        enabled: user.network.sync.tip_poll.enabled,
                        lag_threshold_blocks: user.network.sync.tip_poll.lag_threshold_blocks,
                        max_peers_to_sample: user.network.sync.tip_poll.max_peers_to_sample,
                    },
                },
            };
            let chain_leader_settings = lb_chain_leader_service::LeaderSettings {
                config: ledger_config,
                wallet_config: lb_chain_leader_service::LeaderWalletConfig {
                    funding_pk: user.leader.wallet.funding_pk,
                    max_tx_fee: user.leader.wallet.max_tx_fee,
                },
            };
            (
                chain_service_settings,
                chain_network_settings,
                chain_leader_settings,
            )
        })
    }
}
