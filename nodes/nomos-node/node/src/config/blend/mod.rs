use nomos_blend::scheduling::message_blend::crypto::SessionCryptographicProcessorSettings;
use nomos_blend_service::{
    core::{
        backends::libp2p::Libp2pBlendBackendSettings as Libp2pCoreBlendBackendSettings,
        settings::BlendConfig as BlendCoreSettings,
    },
    edge::{
        backends::libp2p::Libp2pBlendBackendSettings as Libp2pEdgeBlendBackendSettings,
        settings::BlendConfig as BlendEdgeSettings,
    },
    settings::{CommonSettings, CoreSettings, EdgeSettings, Settings as BlendSettings},
};

use crate::config::blend::{deployment::Settings as DeploymentSettings, serde::Config};

pub mod deployment;
pub mod serde;

/// Blend service config which combines user-provided configuration with
/// deployment-specific settings.
///
/// Deployment-specific settings can refer to either a well-known deployment
/// (e.g., Nomos Mainnet), or to custom values.
pub struct ServiceConfig {
    pub user: Config,
    pub deployment: DeploymentSettings,
}

impl From<ServiceConfig>
    for (
        BlendSettings<Libp2pCoreBlendBackendSettings, Libp2pEdgeBlendBackendSettings>,
        BlendCoreSettings<Libp2pCoreBlendBackendSettings>,
        BlendEdgeSettings<Libp2pEdgeBlendBackendSettings>,
    )
{
    fn from(config: ServiceConfig) -> Self {
        let blend_service_settings = BlendSettings::<
            Libp2pCoreBlendBackendSettings,
            Libp2pEdgeBlendBackendSettings,
        > {
            common: CommonSettings {
                crypto: SessionCryptographicProcessorSettings {
                    non_ephemeral_signing_key: config.user.non_ephemeral_signing_key,
                    num_blend_layers: config.deployment.common.num_blend_layers,
                },
                minimum_network_size: config.deployment.common.minimum_network_size,
                recovery_path_prefix: config.user.recovery_path_prefix,
                time: config.deployment.common.timing,
            },
            core: CoreSettings {
                backend: Libp2pCoreBlendBackendSettings {
                    core_peering_degree: config.user.core.backend.core_peering_degree,
                    listening_address: config.user.core.backend.listening_address,
                    edge_node_connection_timeout: config
                        .user
                        .core
                        .backend
                        .edge_node_connection_timeout,
                    max_dial_attempts_per_peer: config.user.core.backend.max_dial_attempts_per_peer,
                    max_edge_node_incoming_connections: config
                        .user
                        .core
                        .backend
                        .max_edge_node_incoming_connections,
                    minimum_messages_coefficient: config
                        .deployment
                        .core
                        .minimum_messages_coefficient,
                    normalization_constant: config.deployment.core.normalization_constant,
                    protocol_name: config.deployment.common.protocol_name.clone(),
                },
                scheduler: config.deployment.core.scheduler,
                zk: config.user.core.zk,
            },
            edge: EdgeSettings::<Libp2pEdgeBlendBackendSettings> {
                backend: Libp2pEdgeBlendBackendSettings {
                    max_dial_attempts_per_peer_per_message: config
                        .user
                        .edge
                        .backend
                        .max_dial_attempts_per_peer_per_message,
                    protocol_name: config.deployment.common.protocol_name,
                    replication_factor: config.user.edge.backend.replication_factor,
                },
            },
        };
        let blend_core_settings: BlendCoreSettings<_> = blend_service_settings.clone().into();
        let blend_edge_settings: BlendEdgeSettings<_> = blend_service_settings.clone().into();
        (
            blend_service_settings,
            blend_core_settings,
            blend_edge_settings,
        )
    }
}
