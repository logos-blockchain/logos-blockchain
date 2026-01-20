use lb_blend_service::core::network::libp2p::Libp2pBroadcastSettings;
use lb_chain_network_service::network::adapters::libp2p::LibP2pAdapterSettings;
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
        let ledger_config = lb_ledger::Config {
            consensus_config: self.deployment.consensus_config,
            epoch_config: self.deployment.epoch_config,
            sdp_config: lb_ledger::mantle::sdp::Config {
                min_stake: self.deployment.sdp_config.min_stake,
                service_params: self.deployment.sdp_config.service_params,
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
            config: ledger_config.clone(),
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
            leader_config: self.user.leader,
            transaction_selector_settings: (),
        };
        (
            chain_service_settings,
            chain_network_settings,
            chain_leader_settings,
        )
    }
}
