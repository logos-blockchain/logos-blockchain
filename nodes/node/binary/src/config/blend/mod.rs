use lb_blend_service::{
    core::{
        backends::libp2p::Libp2pBlendBackendSettings as Libp2pCoreBlendBackendSettings,
        settings::{
            CoverTrafficSettings, MessageDelayerSettings, SchedulerSettings,
            StartingBlendConfig as BlendCoreSettings, ZkSettings,
        },
    },
    edge::{
        backends::libp2p::Libp2pBlendBackendSettings as Libp2pEdgeBlendBackendSettings,
        settings::StartingBlendConfig as BlendEdgeSettings,
    },
    settings::{
        CommonSettings, CoreSettings, EdgeSettings, Settings as BlendSettings, TimingSettings,
    },
};

use crate::config::blend::{deployment::Settings as DeploymentSettings, serde::Config};

pub mod deployment;
pub mod serde;

/// Blend service config which combines user-provided configuration with
/// deployment-specific settings.
///
/// Deployment-specific settings can refer to either a well-known deployment
/// (e.g., Logos blockchain Mainnet), or to custom values.
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
    #[expect(clippy::too_many_lines, reason = "From implementation.")]
    fn from(config: ServiceConfig) -> Self {
        let blend_service_settings = BlendSettings::<
            Libp2pCoreBlendBackendSettings,
            Libp2pEdgeBlendBackendSettings,
        > {
            common: CommonSettings {
                non_ephemeral_signing_key_id: config.user.non_ephemeral_signing_key_id,
                num_blend_layers: config.deployment.common.num_blend_layers,
                minimum_network_size: config.deployment.common.minimum_network_size,
                recovery_path_prefix: config.user.recovery_path_prefix,
                time: TimingSettings {
                    epoch_transition_period_in_slots: config
                        .deployment
                        .common
                        .timing
                        .epoch_transition_period_in_slots,
                    round_duration: config.deployment.common.timing.round_duration,
                    rounds_per_interval: config.deployment.common.timing.rounds_per_interval,
                    rounds_per_observation_window: config
                        .deployment
                        .common
                        .timing
                        .rounds_per_observation_window,
                    rounds_per_session: config.deployment.common.timing.rounds_per_session,
                    rounds_per_session_transition_period: config
                        .deployment
                        .common
                        .timing
                        .rounds_per_session_transition_period,
                },
                data_replication_factor: config.deployment.common.data_replication_factor,
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
                scheduler: SchedulerSettings {
                    cover: CoverTrafficSettings {
                        intervals_for_safety_buffer: config
                            .deployment
                            .core
                            .scheduler
                            .cover
                            .intervals_for_safety_buffer,
                        message_frequency_per_round: config
                            .deployment
                            .core
                            .scheduler
                            .cover
                            .message_frequency_per_round,
                    },
                    delayer: MessageDelayerSettings {
                        maximum_release_delay_in_rounds: config
                            .deployment
                            .core
                            .scheduler
                            .delayer
                            .maximum_release_delay_in_rounds,
                    },
                },
                zk: ZkSettings {
                    secret_key_kms_id: config.user.core.zk.secret_key_kms_id,
                },
                activity_threshold_sensitivity: config
                    .deployment
                    .core
                    .activity_threshold_sensitivity,
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
