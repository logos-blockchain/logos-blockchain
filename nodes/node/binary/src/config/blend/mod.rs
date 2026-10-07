use core::time::Duration;

use lb_blend_service::{
    broadcast::settings::EraSettings as BlendBroadcastSettings,
    core::{
        backends::libp2p::Libp2pBlendBackendSettings as Libp2pCoreBlendBackendSettings,
        dispatcher::libp2p::Libp2pBroadcastSettings,
        settings::{
            CoreServiceSettings as BlendCoreSettings, CoverTrafficSettings, MessageDelayerSettings,
            SchedulerSettings, ZkSettings,
        },
    },
    edge::{
        backends::libp2p::Libp2pBlendBackendSettings as Libp2pEdgeBlendBackendSettings,
        settings::EraSettings as BlendEdgeSettings,
    },
    settings::{
        CommonSettings, CoreSettings, EdgeSettings, EraSettings as BlendSettings, Settings,
        TimingSettings,
    },
};
use lb_cryptarchia_engine::era::EraSchedule;
use lb_era_parameters::{EraDefinition, EraParameters, v1};
use lb_services_utils::overwatch::RecoveryData;

use crate::config::blend::serde::Config;

pub mod serde;

/// The settings of the Blend services in an era, on the libp2p backends.
type Libp2pBlendSettings = BlendSettings<
    Libp2pCoreBlendBackendSettings,
    Libp2pEdgeBlendBackendSettings,
    Libp2pBroadcastSettings,
>;

/// The settings the Blend services run, each in every era: the proxy's, which
/// picks between the others, and one for each of the services it can start.
type BlendServicesSettings = (
    EraSchedule<Libp2pBlendSettings>,
    BlendCoreSettings<Libp2pCoreBlendBackendSettings, Libp2pBroadcastSettings>,
    EraSchedule<BlendEdgeSettings<Libp2pEdgeBlendBackendSettings, Libp2pBroadcastSettings>>,
    EraSchedule<BlendBroadcastSettings<Libp2pBroadcastSettings>>,
);

/// Blend service config: the user-provided configuration, completed with the
/// chain's eras.
pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    #[must_use]
    pub fn into_blend_services_settings(
        self,
        eras: &EraSchedule<EraDefinition>,
        recovery_data: RecoveryData,
    ) -> BlendServicesSettings {
        let blend_settings = eras.map(|era| era_settings(&self.user, &era.entry.parameters, eras));
        let blend_core_settings = BlendCoreSettings {
            eras: blend_settings.map(|era| era.entry.parameters.clone().into()),
            recovery_data,
        };
        let blend_edge_settings = blend_settings.map(|era| era.entry.parameters.clone().into());
        let blend_broadcast_settings =
            blend_settings.map(|era| era.entry.parameters.clone().into());
        (
            blend_settings,
            blend_core_settings,
            blend_edge_settings,
            blend_broadcast_settings,
        )
    }
}

/// The settings of the Blend services while `era` is in force, on a chain
/// whose eras are `eras`, for a node configured with `user`: the version of
/// Blend each global version runs.
fn era_settings(
    user: &Config,
    era: &EraDefinition,
    eras: &EraSchedule<EraDefinition>,
) -> Libp2pBlendSettings {
    match &era.parameters {
        EraParameters::V1(parameters) => {
            BlendSettings::V1(v1_settings(user, parameters, era, eras))
        }
    }
}

/// Version 1 of the Blend settings, from the parameters of an era of version
/// 1: Blend's own, and the slot and epoch lengths its timing follows.
fn v1_settings(
    user: &Config,
    parameters: &v1::Parameters,
    era: &EraDefinition,
    eras: &EraSchedule<EraDefinition>,
) -> Settings<Libp2pCoreBlendBackendSettings, Libp2pEdgeBlendBackendSettings, Libp2pBroadcastSettings>
{
    let blend = &parameters.blend;
    let slots_per_epoch = parameters.cryptarchia.slots_per_epoch();
    let slots_per_block = parameters.cryptarchia.average_slots_per_block();
    let slot_duration = parameters.time.slot_duration;
    let protocol_name = era.protocol_names.blend.clone();

    Settings {
        common: CommonSettings {
            non_ephemeral_signing_key_id: user.non_ephemeral_signing_key_id.clone(),
            num_blend_layers: blend.common.num_blend_layers,
            minimum_network_size: blend.common.minimum_network_size.into(),
            // A proposal goes out on the topic of its own era, which is not
            // always the era in force.
            broadcast: Libp2pBroadcastSettings {
                topics: eras.map(|era| {
                    era.entry
                        .parameters
                        .protocol_names
                        .cryptarchia_topic
                        .clone()
                }),
            },
            abstain_on_failure: user.abstain_on_failure,
            time: timing_settings(blend, slots_per_epoch, slots_per_block, &slot_duration),
            data_replication_factor: blend.common.data_replication_factor,
        },
        core: CoreSettings {
            backend: Libp2pCoreBlendBackendSettings {
                target_peering_degree: blend.core.target_peering_degree,
                connection_share_per_round: blend.connection_share_per_round(),
                listening_address: user.core.backend.listening_address.clone(),
                edge_node_connection_timeout: blend.edge_node_connection_timeout(&slot_duration),
                max_dial_attempts_per_peer: user.core.backend.max_dial_attempts_per_peer,
                max_edge_node_incoming_connections: blend.maximum_concurrent_edge_connections(),
                accepted_edge_connections_per_round: blend.accepted_edge_connections_per_round(),
                protocol_name: protocol_name.clone(),
                peering_degree_check_interval: user.core.backend.peering_degree_check_interval,
            },
            scheduler: SchedulerSettings {
                cover: CoverTrafficSettings {
                    message_frequency_per_round: blend
                        .core
                        .scheduler
                        .cover
                        .message_frequency_per_round,
                },
                delayer: MessageDelayerSettings {
                    maximum_release_delay_in_rounds: blend
                        .core
                        .scheduler
                        .delayer
                        .maximum_release_delay_in_rounds,
                },
            },
            zk: ZkSettings {
                secret_key_kms_id: user.core.zk.secret_key_kms_id.clone(),
            },
            activity_threshold_sensitivity: blend.core.activity_threshold_sensitivity,
        },
        edge: EdgeSettings {
            backend: Libp2pEdgeBlendBackendSettings {
                max_dial_attempts_per_peer_per_message: user
                    .edge
                    .backend
                    .max_dial_attempts_per_peer_per_message,
                protocol_name,
                replication_factor: user.edge.backend.replication_factor,
            },
        },
    }
}

fn timing_settings(
    blend: &v1::blend::Settings,
    slots_per_epoch: u64,
    slots_per_block: u64,
    slot_duration: &Duration,
) -> TimingSettings {
    TimingSettings {
        epoch_transition_period: blend.epoch_transition(slots_per_block, slot_duration),
        round_duration_in_seconds: blend
            .round_duration(slot_duration)
            .as_secs()
            .try_into()
            .expect("Round duration must be greater than `0` seconds."),
        rounds_per_observation_window: blend.rounds_per_observation_window(),
        network_absorption_in_rounds: blend.common.network_absorption_in_rounds,
        core_handshake_deadline_in_rounds: blend.core.core_handshake_deadline_in_rounds,
        rounds_per_epoch: blend.rounds_per_epoch(slots_per_epoch, slot_duration),
    }
}
