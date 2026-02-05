use std::sync::Arc;

use lb_blend_service::core::network::libp2p::Libp2pBroadcastSettings;
use lb_chain_network_service::network::adapters::libp2p::LibP2pAdapterSettings;
use lb_core::sdp::ServiceParameters;
use lb_ledger::mantle::sdp::{ServiceRewardsParameters, rewards::blend::RewardsParameters};
use lb_libp2p::PeerId;

use crate::config::{
    blend::deployment::Settings as BlendDeploymentSettings,
    cryptarchia::{deployment::Settings as DeploymentSettings, serde::Config},
};

pub mod deployment;
pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
    pub deployment: DeploymentSettings,
}

impl ServiceConfig {
    #[must_use]
    pub fn into_cryptarchia_services_settings(
        self,
        blend_deployment: &BlendDeploymentSettings,
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
            epoch_config: self.deployment.epoch_config,
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
            bootstrap: self.user.service.bootstrap,
            config: ledger_config.clone(),
            recovery_file: self.user.service.recovery_file,
            starting_state: self.user.service.starting_state,
        };
        let chain_network_settings = lb_chain_network_service::ChainNetworkSettings {
            bootstrap: self.user.network.bootstrap,
            network_adapter_settings: LibP2pAdapterSettings {
                topic: self.deployment.gossipsub_protocol.clone(),
            },
            sync: self.user.network.sync,
        };
        let chain_leader_settings = lb_chain_leader_service::LeaderSettings {
            blend_broadcast_settings: Libp2pBroadcastSettings {
                topic: self.deployment.gossipsub_protocol,
            },
            config: ledger_config,
            transaction_selector_settings: (),
            wallet_config: self.user.leader.wallet,
        };
        (
            chain_service_settings,
            chain_network_settings,
            chain_leader_settings,
        )
    }
}
