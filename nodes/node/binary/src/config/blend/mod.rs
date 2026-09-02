use lb_blend_service::{
    core::{
        backends::libp2p::Libp2pBlendBackendSettings as Libp2pCoreBlendBackendSettings,
        dispatcher::libp2p::Libp2pBroadcastSettings,
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
use lb_services_utils::overwatch::RecoveryData;

use crate::config::{
    blend::{deployment::Settings as DeploymentSettings, serde::Config},
    cryptarchia::deployment::Settings as CryptarchiaDeploymentSettings,
    time::deployment::Settings as TimeDeploymentSettings,
};

pub mod deployment;
pub mod serde;

/// The three settings a Blend deployment produces: the proxy's, which picks
/// between the two, and one for each of the services it can start.
type BlendServicesSettings = (
    BlendSettings<
        Libp2pCoreBlendBackendSettings,
        Libp2pEdgeBlendBackendSettings,
        Libp2pBroadcastSettings,
    >,
    BlendCoreSettings<Libp2pCoreBlendBackendSettings, Libp2pBroadcastSettings>,
    BlendEdgeSettings<Libp2pEdgeBlendBackendSettings, Libp2pBroadcastSettings>,
);

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
        recovery_data: RecoveryData,
        time_deployment: &TimeDeploymentSettings,
        cryptarchia_deployment: &CryptarchiaDeploymentSettings,
    ) -> BlendServicesSettings {
        let slots_per_epoch = cryptarchia_deployment.slots_per_epoch();
        let slots_per_block = cryptarchia_deployment.average_slots_per_block();
        let slot_duration = time_deployment.slot_duration;

        let blend_service_settings = BlendSettings::<
            Libp2pCoreBlendBackendSettings,
            Libp2pEdgeBlendBackendSettings,
            Libp2pBroadcastSettings,
        > {
            common: CommonSettings {
                non_ephemeral_signing_key_id: self.user.non_ephemeral_signing_key_id,
                num_blend_layers: self.deployment.common.num_blend_layers,
                minimum_network_size: self.deployment.common.minimum_network_size.into(),
                // Where Blend hands a block proposal over to the rest of the
                // node, and where a sender watches for its own.
                broadcast: Libp2pBroadcastSettings {
                    topic: cryptarchia_deployment.gossipsub_protocol.clone(),
                },
                blend_failure_fallback: self.user.blend_failure_fallback,
                recovery_data,
                time: TimingSettings {
                    epoch_transition_period: self
                        .deployment
                        .epoch_transition(slots_per_block, &slot_duration),
                    round_duration: self.deployment.round_duration(&slot_duration),
                    rounds_per_observation_window: self.deployment.rounds_per_observation_window(),
                    rounds_per_epoch: self
                        .deployment
                        .rounds_per_epoch(slots_per_epoch, &slot_duration),
                    delivery_deadline: self.deployment.max_data_message_delay_in_rounds(),
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
                    peering_degree_check_interval: self
                        .user
                        .core
                        .backend
                        .peering_degree_check_interval,
                },
                scheduler: SchedulerSettings {
                    cover: CoverTrafficSettings {
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
        let blend_core_settings: BlendCoreSettings<_, _> = blend_service_settings.clone().into();
        let blend_edge_settings: BlendEdgeSettings<_, _> = blend_service_settings.clone().into();
        (
            blend_service_settings,
            blend_core_settings,
            blend_edge_settings,
        )
    }
}
