use lb_blend_service::{
    broadcast::settings::{BroadcastServiceEraSettings, BroadcastServiceSettings},
    core::{
        backends::libp2p::{
            Libp2pBlendBackendEraSettings as Libp2pCoreBlendBackendEraSettings,
            Libp2pBlendBackendSettings as Libp2pCoreBlendBackendSettings,
        },
        dispatcher::libp2p::Libp2pBroadcastSettings,
        settings::{
            CoreServiceEraSettings, CoreServiceSettings, EraCoverTrafficSettings,
            EraMessageDelayerSettings, EraSchedulerSettings, ZkSettings,
        },
    },
    edge::{
        backends::libp2p::{
            Libp2pBlendBackendEraSettings as Libp2pEdgeBlendBackendEraSettings,
            Libp2pBlendBackendSettings as Libp2pEdgeBlendBackendSettings,
        },
        settings::{EdgeServiceEraSettings, EdgeServiceSettings},
    },
    settings::{EraOrchestratorSettings, OrchestratorSettings},
};
use lb_libp2p::protocol_name::ProtocolScope;
use lb_services_utils::overwatch::RecoveryData;
use lb_time::era::EraSchedule;

use crate::config::{
    blend::serde::Config,
    deployment::{EraDefinition, era::ruleset::EraRuleset},
};

pub mod serde;

/// The libp2p dispatcher's settings: the topic of every era.
type DispatcherSettings = EraSchedule<Libp2pBroadcastSettings>;

/// The settings of the four Blend services: the orchestrator's, which picks the
/// mode each epoch runs in, and one for each of the services it can start.
type BlendServicesSettings = (
    OrchestratorSettings,
    CoreServiceSettings<Libp2pCoreBlendBackendSettings, DispatcherSettings>,
    EdgeServiceSettings<
        Libp2pEdgeBlendBackendSettings,
        Libp2pEdgeBlendBackendEraSettings,
        DispatcherSettings,
    >,
    BroadcastServiceSettings<DispatcherSettings>,
);

/// What an era holds for the Blend services, and for the libp2p backends and
/// dispatcher they run with, each in a type of its own.
struct BlendEraSettings {
    orchestrator: EraOrchestratorSettings,
    core: CoreServiceEraSettings,
    edge: EdgeServiceEraSettings<Libp2pEdgeBlendBackendEraSettings>,
    broadcast: BroadcastServiceEraSettings,
    core_backend: Libp2pCoreBlendBackendEraSettings,
    dispatcher: Libp2pBroadcastSettings,
}

/// Blend service config: the user-provided configuration, completed with the
/// parameters of each era when the services' settings are built.
pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    /// The settings of the Blend services in every era of `eras`.
    #[must_use]
    #[expect(
        clippy::too_many_lines,
        reason = "Era -> service config conversion function. Better to have all the logic in a single place."
    )]
    pub fn into_blend_services_settings(
        self,
        recovery_data: RecoveryData,
        eras: &EraSchedule<EraDefinition>,
    ) -> BlendServicesSettings {
        let blend_eras = eras.map(|era| {
            let definition = &era.parameters;
            let EraRuleset::V1(parameters) = &definition.ruleset;
            let fork = ProtocolScope::Fork(definition.fork_digest);
            let protocol_name = fork.to_stream_protocol_with_name("blend");
            let (deployment, cryptarchia_deployment, time_deployment) =
                (&parameters.blend, &parameters.cryptarchia, &parameters.time);
            let slots_per_epoch = cryptarchia_deployment.slots_per_epoch();
            let slots_per_block = cryptarchia_deployment.average_slots_per_block();
            let slot_duration = time_deployment.slot_duration();
            let num_blend_layers = deployment.common.num_blend_layers;
            let minimum_network_size = deployment.common.minimum_network_size.into();
            let time = deployment.timing_settings(slots_per_epoch, slots_per_block, &slot_duration);
            let data_replication_factor = deployment.common.data_replication_factor;
            let epoch_transition_period = time.epoch_transition_period;

            let core = CoreServiceEraSettings {
                scheduler: EraSchedulerSettings {
                    cover: EraCoverTrafficSettings {
                        message_frequency_per_round: deployment
                            .core
                            .scheduler
                            .cover
                            .message_frequency_per_round,
                    },
                    delayer: EraMessageDelayerSettings {
                        maximum_release_delay_in_rounds: deployment
                            .core
                            .scheduler
                            .delayer
                            .maximum_release_delay_in_rounds,
                    },
                },
                time: time.clone(),
                num_blend_layers,
                minimum_network_size,
                data_replication_factor,
                activity_threshold_sensitivity: deployment.core.activity_threshold_sensitivity,
            };
            let edge = EdgeServiceEraSettings {
                time: time.clone(),
                num_blend_layers,
                minimum_network_size,
                cover: core.scheduler.cover.clone(),
                data_replication_factor,
                max_blend_delay_in_rounds: core.scheduler.delayer.maximum_release_delay_in_rounds,
                backend: Libp2pEdgeBlendBackendEraSettings {
                    protocol_name: protocol_name.clone(),
                },
            };

            BlendEraSettings {
                orchestrator: EraOrchestratorSettings {
                    minimum_network_size,
                    epoch_transition_period,
                },
                core,
                edge,
                broadcast: BroadcastServiceEraSettings {
                    minimum_network_size,
                    epoch_transition_period,
                },
                core_backend: Libp2pCoreBlendBackendEraSettings {
                    target_peering_degree: deployment.core.target_peering_degree,
                    connection_share_per_round: deployment.connection_share_per_round(),
                    edge_node_connection_timeout: deployment
                        .edge_node_connection_timeout(&slot_duration),
                    max_edge_node_incoming_connections: deployment
                        .maximum_concurrent_edge_connections(),
                    accepted_edge_connections_per_round: deployment
                        .accepted_edge_connections_per_round(),
                    protocol_name,
                    minimum_network_size,
                    num_blend_layers,
                    time,
                },
                dispatcher: Libp2pBroadcastSettings {
                    topic: fork.to_string_with_name("cryptarchia"),
                },
            }
        });
        let user = self.user;
        let dispatcher_settings = blend_eras.map(|era| era.parameters.dispatcher.clone());
        let blend_orchestrator_settings = OrchestratorSettings {
            non_ephemeral_signing_key_id: user.non_ephemeral_signing_key_id.clone(),
            zk_secret_key_kms_id: user.core.zk.secret_key_kms_id.clone(),
            era_schedule: blend_eras.map(|era| era.parameters.orchestrator.clone()),
        };
        let blend_core_settings = CoreServiceSettings {
            non_ephemeral_signing_key_id: user.non_ephemeral_signing_key_id.clone(),
            zk: ZkSettings {
                secret_key_kms_id: user.core.zk.secret_key_kms_id,
            },
            abstain_on_failure: user.abstain_on_failure,
            backend: Libp2pCoreBlendBackendSettings {
                listening_address: user.core.backend.listening_address,
                max_dial_attempts_per_peer: user.core.backend.max_dial_attempts_per_peer,
                peering_degree_check_interval: user.core.backend.peering_degree_check_interval,
                era_schedule: blend_eras.map(|era| era.parameters.core_backend.clone()),
            },
            dispatcher: dispatcher_settings.clone(),
            era_schedule: blend_eras.map(|era| era.parameters.core.clone()),
            recovery_data,
        };
        let blend_edge_settings = EdgeServiceSettings {
            non_ephemeral_signing_key_id: user.non_ephemeral_signing_key_id.clone(),
            abstain_on_failure: user.abstain_on_failure,
            backend: Libp2pEdgeBlendBackendSettings {
                max_dial_attempts_per_peer_per_message: user
                    .edge
                    .backend
                    .max_dial_attempts_per_peer_per_message,
                replication_factor: user.edge.backend.replication_factor,
            },
            dispatcher: dispatcher_settings.clone(),
            era_schedule: blend_eras.map(|era| era.parameters.edge.clone()),
        };
        let blend_broadcast_settings = BroadcastServiceSettings {
            non_ephemeral_signing_key_id: user.non_ephemeral_signing_key_id,
            dispatcher: dispatcher_settings,
            era_schedule: blend_eras.map(|era| era.parameters.broadcast.clone()),
        };
        (
            blend_orchestrator_settings,
            blend_core_settings,
            blend_edge_settings,
            blend_broadcast_settings,
        )
    }
}
