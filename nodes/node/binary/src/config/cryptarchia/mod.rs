use std::{path::PathBuf, sync::Arc};

use lb_blend_service::core::network::libp2p::Libp2pBroadcastSettings;
use lb_chain_network_service::network::adapters::libp2p::LibP2pAdapterSettings;
use lb_core::sdp::ServiceParameters;
use lb_cryptarchia_engine::EpochConfig;
use lb_ledger::mantle::sdp::{ServiceRewardsParameters, rewards::blend::RewardsParameters};
use lb_libp2p::PeerId;

use crate::config::{
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
        blend_rewards_params: RewardsParameters,
        state_config: &StateConfig,
    ) -> (
        lb_chain_service::CryptarchiaSettings,
        lb_chain_network_service::ChainNetworkSettings<PeerId, LibP2pAdapterSettings>,
        lb_chain_leader_service::LeaderSettings<(), Libp2pBroadcastSettings>,
    ) {
        let blocks_per_session = self.deployment.blocks_per_epoch();

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
                                    session_duration: blocks_per_session,
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
                    blend: blend_rewards_params,
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
            // TODO: Change starting state to be genesis block.
            starting_state: self
                .deployment
                .genesis_block
                .into_transactions()
                .into_iter()
                .next()
                .expect("genesis block always contains exactly one transaction")
                .into(),
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
