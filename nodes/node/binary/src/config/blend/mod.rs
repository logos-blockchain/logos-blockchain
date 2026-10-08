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
    settings::{CommonSettings, CoreSettings, EdgeSettings, Settings as BlendSettings},
};
use lb_cryptarchia_engine::era::EraSchedule;
use lb_services_utils::overwatch::RecoveryData;

use crate::config::{
    blend::serde::Config,
    deployment::{EraDefinition, ProtocolScope, era::parameters::EraParameters},
};

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

/// Blend service config: the user-provided configuration, completed with the
/// parameters of each era when the services' settings are built.
pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    /// The settings of the Blend services in every era of `eras`.
    #[must_use]
    pub fn into_blend_services_era_schedule(
        self,
        recovery_data: RecoveryData,
        eras: &EraSchedule<EraDefinition>,
    ) -> EraSchedule<BlendServicesSettings> {
        eras.map(|era| {
            let user = self.user.clone();
            let definition = &era.entry.parameters;
            let EraParameters::V1(parameters) = &definition.parameters;
            let fork = ProtocolScope::Fork(definition.fork_digest);
            let protocol_name = fork.to_stream_protocol_with_name("blend");
            let (deployment, cryptarchia_deployment, time_deployment) =
                (&parameters.blend, &parameters.cryptarchia, &parameters.time);
            let slots_per_epoch = cryptarchia_deployment.slots_per_epoch();
            let slots_per_block = cryptarchia_deployment.average_slots_per_block();
            let slot_duration = time_deployment.slot_duration;

            let blend_service_settings = BlendSettings::<
                Libp2pCoreBlendBackendSettings,
                Libp2pEdgeBlendBackendSettings,
                Libp2pBroadcastSettings,
            > {
                common: CommonSettings {
                    non_ephemeral_signing_key_id: user.non_ephemeral_signing_key_id,
                    num_blend_layers: deployment.common.num_blend_layers,
                    minimum_network_size: deployment.common.minimum_network_size.into(),
                    broadcast: Libp2pBroadcastSettings {
                        topic: fork.to_string_with_name("cryptarchia"),
                    },
                    abstain_on_failure: user.abstain_on_failure,
                    recovery_data: recovery_data.clone(),
                    time: deployment.timing_settings(
                        slots_per_epoch,
                        slots_per_block,
                        &slot_duration,
                    ),
                    data_replication_factor: deployment.common.data_replication_factor,
                },
                core: CoreSettings {
                    backend: Libp2pCoreBlendBackendSettings {
                        target_peering_degree: deployment.core.target_peering_degree,
                        connection_share_per_round: deployment.connection_share_per_round(),
                        listening_address: user.core.backend.listening_address,
                        edge_node_connection_timeout: deployment
                            .edge_node_connection_timeout(&slot_duration),
                        max_dial_attempts_per_peer: user.core.backend.max_dial_attempts_per_peer,
                        max_edge_node_incoming_connections: deployment
                            .maximum_concurrent_edge_connections(),
                        accepted_edge_connections_per_round: deployment
                            .accepted_edge_connections_per_round(),
                        protocol_name: protocol_name.clone(),
                        peering_degree_check_interval: user
                            .core
                            .backend
                            .peering_degree_check_interval,
                    },
                    scheduler: SchedulerSettings {
                        cover: CoverTrafficSettings {
                            message_frequency_per_round: deployment
                                .core
                                .scheduler
                                .cover
                                .message_frequency_per_round,
                        },
                        delayer: MessageDelayerSettings {
                            maximum_release_delay_in_rounds: deployment
                                .core
                                .scheduler
                                .delayer
                                .maximum_release_delay_in_rounds,
                        },
                    },
                    zk: ZkSettings {
                        secret_key_kms_id: user.core.zk.secret_key_kms_id,
                    },
                    activity_threshold_sensitivity: deployment.core.activity_threshold_sensitivity,
                },
                edge: EdgeSettings::<Libp2pEdgeBlendBackendSettings> {
                    backend: Libp2pEdgeBlendBackendSettings {
                        max_dial_attempts_per_peer_per_message: user
                            .edge
                            .backend
                            .max_dial_attempts_per_peer_per_message,
                        protocol_name,
                        replication_factor: user.edge.backend.replication_factor,
                    },
                },
            };
            let blend_core_settings: BlendCoreSettings<_, _> =
                blend_service_settings.clone().into();
            let blend_edge_settings: BlendEdgeSettings<_, _> =
                blend_service_settings.clone().into();
            (
                blend_service_settings,
                blend_core_settings,
                blend_edge_settings,
            )
        })
    }
}
