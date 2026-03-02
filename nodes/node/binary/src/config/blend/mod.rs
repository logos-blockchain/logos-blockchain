use std::path::Path;

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

use crate::config::{
    blend::{deployment::Settings as DeploymentSettings, serde::Config},
    cryptarchia::deployment::Settings as CryptarchiaDeploymentSettings,
    state::Config as StateConfig,
    time::deployment::Settings as TimeDeploymentSettings,
};

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

impl ServiceConfig {
    #[must_use]
    pub fn into_blend_services_settings(
        self,
        state_config: &StateConfig,
        time_deployment: &TimeDeploymentSettings,
        cryptarchia_deployment: &CryptarchiaDeploymentSettings,
    ) -> (
        BlendSettings<Libp2pCoreBlendBackendSettings, Libp2pEdgeBlendBackendSettings>,
        BlendCoreSettings<Libp2pCoreBlendBackendSettings>,
        BlendEdgeSettings<Libp2pEdgeBlendBackendSettings>,
    ) {
        let recovery_path_prefix = state_config.get_path_for_recovery_state(Path::new("blend"));

        let slots_per_epoch = cryptarchia_deployment.slots_per_epoch();
        let slots_per_block = cryptarchia_deployment.average_slots_per_block();
        let slot_duration = time_deployment.slot_duration;

        let blend_service_settings = BlendSettings::<
            Libp2pCoreBlendBackendSettings,
            Libp2pEdgeBlendBackendSettings,
        > {
            common: CommonSettings {
                non_ephemeral_signing_key_id: self.user.non_ephemeral_signing_key_id,
                num_blend_layers: self.deployment.common.num_blend_layers,
                minimum_network_size: self.deployment.common.minimum_network_size,
                recovery_path_prefix,
                time: TimingSettings {
                    epoch_transition_period_in_slots: self
                        .deployment
                        .slots_per_epoch_transition_period(slots_per_block, &slot_duration),
                    round_duration: self.deployment.round_duration(&slot_duration),
                    rounds_per_interval: self
                        .deployment
                        .rounds_per_interval(slots_per_block, &slot_duration),
                    rounds_per_observation_window: self.deployment.rounds_per_observation_window(),
                    rounds_per_session: self
                        .deployment
                        .rounds_per_session(slots_per_epoch, &slot_duration),
                    rounds_per_session_transition_period: self
                        .deployment
                        .rounds_per_session_transition_period(slots_per_block, &slot_duration),
                },
                data_replication_factor: self.deployment.common.data_replication_factor,
            },
            core: CoreSettings {
                backend: Libp2pCoreBlendBackendSettings {
                    core_peering_degree: self.user.core.backend.core_peering_degree,
                    listening_address: self.user.core.backend.listening_address,
                    edge_node_connection_timeout: self
                        .user
                        .core
                        .backend
                        .edge_node_connection_timeout,
                    max_dial_attempts_per_peer: self.user.core.backend.max_dial_attempts_per_peer,
                    max_edge_node_incoming_connections: self
                        .user
                        .core
                        .backend
                        .max_edge_node_incoming_connections,
                    minimum_messages_coefficient: self.deployment.core.minimum_messages_coefficient,
                    normalization_constant: self.deployment.core.normalization_constant,
                    protocol_name: self.deployment.common.protocol_name.clone(),
                },
                scheduler: SchedulerSettings {
                    cover: CoverTrafficSettings {
                        intervals_for_safety_buffer: self
                            .deployment
                            .core
                            .scheduler
                            .cover
                            .intervals_for_safety_buffer,
                        message_frequency_per_round: self
                            .deployment
                            .core
                            .scheduler
                            .cover
                            .message_frequency_per_round,
                    },
                    delayer: MessageDelayerSettings {
                        maximum_release_delay_in_rounds: self
                            .deployment
                            .core
                            .scheduler
                            .delayer
                            .maximum_release_delay_in_rounds,
                    },
                },
                zk: ZkSettings {
                    secret_key_kms_id: self.user.core.zk.secret_key_kms_id,
                },
                activity_threshold_sensitivity: self.deployment.core.activity_threshold_sensitivity,
            },
            edge: EdgeSettings::<Libp2pEdgeBlendBackendSettings> {
                backend: Libp2pEdgeBlendBackendSettings {
                    max_dial_attempts_per_peer_per_message: self
                        .user
                        .edge
                        .backend
                        .max_dial_attempts_per_peer_per_message,
                    protocol_name: self.deployment.common.protocol_name,
                    replication_factor: self.user.edge.backend.replication_factor,
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
