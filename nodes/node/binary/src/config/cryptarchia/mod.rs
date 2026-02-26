use std::{path::PathBuf, sync::Arc};

use lb_blend_service::core::network::libp2p::Libp2pBroadcastSettings;
use lb_chain_network_service::network::adapters::libp2p::LibP2pAdapterSettings;
use lb_core::sdp::ServiceParameters;
use lb_cryptarchia_engine::EpochConfig;
use lb_ledger::mantle::sdp::{ServiceRewardsParameters, rewards::blend::RewardsParameters};
use lb_libp2p::PeerId;

use crate::config::{
    blend::deployment::Settings as BlendDeploymentSettings,
    cryptarchia::{deployment::Settings as DeploymentSettings, serde::Config},
    state::Config as StateConfig,
};

pub mod deployment;
pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
    pub deployment: DeploymentSettings,
}

impl ServiceConfig {
    #[must_use]
    #[expect(
        clippy::too_many_lines,
        reason = "Conversion. Useful to have in a single place."
    )]
    pub fn into_cryptarchia_services_settings(
        self,
        blend_deployment: &BlendDeploymentSettings,
        state_config: &StateConfig,
    ) -> (
        lb_chain_service::CryptarchiaSettings,
        lb_chain_network_service::ChainNetworkSettings<PeerId, LibP2pAdapterSettings>,
        lb_chain_leader_service::LeaderSettings<(), Libp2pBroadcastSettings>,
    ) {
        let epoch_schedule = u64::from(
            self.deployment.epoch_config.epoch_period_nonce_buffer.get()
                + self
                    .deployment
                    .epoch_config
                    .epoch_period_nonce_stabilization
                    .get()
                + self
                    .deployment
                    .epoch_config
                    .epoch_stake_distribution_stabilization
                    .get(),
        );
        // Session duration is given by epoch schedule * `k` (security parameter).
        let session_duration_in_blocks =
            epoch_schedule * u64::from(self.deployment.security_param.get());
        let ledger_config = lb_ledger::Config {
            consensus_config: self.deployment.consensus_config(),
            epoch_config: EpochConfig {
                epoch_period_nonce_buffer: self.deployment.epoch_config.epoch_period_nonce_buffer,
                epoch_period_nonce_stabilization: self
                    .deployment
                    .epoch_config
                    .epoch_period_nonce_stabilization,
                epoch_stake_distribution_stabilization: self
                    .deployment
                    .epoch_config
                    .epoch_stake_distribution_stabilization,
            },
            faucet_pk: self.deployment.faucet_pk,
            sdp_config: lb_ledger::mantle::sdp::Config {
                min_stake: self.deployment.sdp_config.min_stake,
                service_params: Arc::new(
                    self.deployment
                        .sdp_config
                        .service_params
                        .into_iter()
                        .map(|(service_type, service_params)| {
                            (
                                service_type,
                                ServiceParameters {
                                    session_duration: session_duration_in_blocks,
                                    inactivity_period: service_params.inactivity_period,
                                    lock_period: service_params.lock_period,
                                    retention_period: service_params.retention_period,
                                    timestamp: service_params.timestamp,
                                },
                            )
                        })
                        .collect(),
                ),
                service_rewards_params: ServiceRewardsParameters {
                    blend: RewardsParameters {
                        message_frequency_per_round: blend_deployment
                            .core
                            .scheduler
                            .cover
                            .message_frequency_per_round,
                        minimum_network_size: blend_deployment.common.minimum_network_size,
                        num_blend_layers: blend_deployment.common.num_blend_layers,
                        rounds_per_session: blend_deployment.common.timing.rounds_per_session,
                        data_replication_factor: blend_deployment.common.data_replication_factor,
                        activity_threshold_sensitivity: blend_deployment
                            .core
                            .activity_threshold_sensitivity,
                    },
                },
            },
        };

        let chain_service_settings = lb_chain_service::CryptarchiaSettings {
            bootstrap: lb_chain_service::BootstrapConfig {
                force_bootstrap: self.user.service.bootstrap.force_bootstrap,
                prolonged_bootstrap_period: self.user.service.bootstrap.prolonged_bootstrap_period,
                offline_grace_period: lb_chain_service::OfflineGracePeriodConfig {
                    grace_period: self
                        .user
                        .service
                        .bootstrap
                        .offline_grace_period
                        .grace_period,
                    state_recording_interval: self
                        .user
                        .service
                        .bootstrap
                        .offline_grace_period
                        .state_recording_interval,
                },
            },
            config: ledger_config.clone(),
            recovery_file: state_config.get_path_for_recovery_state(
                PathBuf::new()
                    .join("consensus")
                    .join("chain_service")
                    .with_extension("json")
                    .as_path(),
            ),
            starting_state: self.deployment.genesis_state.into(),
        };
        let chain_network_settings = lb_chain_network_service::ChainNetworkSettings {
            bootstrap: lb_chain_network_service::BootstrapConfig {
                ibd: lb_chain_network_service::IbdConfig {
                    delay_before_new_download: self
                        .user
                        .network
                        .bootstrap
                        .ibd
                        .delay_before_new_download,
                    peers: self.user.network.bootstrap.ibd.peers,
                },
            },
            network: LibP2pAdapterSettings {
                topic: self.deployment.gossipsub_protocol.clone(),
                max_connected_peers_to_try_download: self
                    .user
                    .network
                    .network
                    .max_connected_peers_to_try_download,
                max_discovered_peers_to_try_download: self
                    .user
                    .network
                    .network
                    .max_discovered_peers_to_try_download,
            },
            sync: lb_chain_network_service::SyncConfig {
                orphan: lb_chain_network_service::OrphanConfig {
                    max_orphan_cache_size: self.user.network.sync.orphan.max_orphan_cache_size,
                },
            },
        };
        let chain_leader_settings = lb_chain_leader_service::LeaderSettings {
            blend_broadcast_settings: Libp2pBroadcastSettings {
                topic: self.deployment.gossipsub_protocol,
            },
            config: ledger_config,
            transaction_selector_settings: (),
            wallet_config: lb_chain_leader_service::LeaderWalletConfig {
                funding_pk: self.user.leader.wallet.funding_pk,
                max_tx_fee: self.user.leader.wallet.max_tx_fee,
            },
        };
        (
            chain_service_settings,
            chain_network_settings,
            chain_leader_settings,
        )
    }
}
