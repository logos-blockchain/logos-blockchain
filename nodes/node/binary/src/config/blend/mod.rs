use core::time::Duration;

use lb_blend_service::{
    broadcast::settings::StartingBlendConfig as BlendBroadcastSettings,
    core::{
        backends::libp2p::{
            Libp2pBlendBackendSettings as Libp2pCoreBlendBackendSettings,
            settings::connection_receive_window,
        },
        dispatcher::libp2p::Libp2pBroadcastSettings,
        settings::{
            CoreServiceSettings as BlendCoreSettings, CoverTrafficSettings, MessageDelayerSettings,
            SchedulerSettings, ZkSettings,
        },
    },
    edge::{
        backends::libp2p::Libp2pBlendBackendSettings as Libp2pEdgeBlendBackendSettings,
        settings::StartingBlendConfig as BlendEdgeSettings,
    },
    settings::{CommonSettings, CoreSettings, EdgeSettings, Settings, TimingSettings},
};
use lb_cryptarchia_engine::era::EraSchedule;
use lb_services_utils::overwatch::RecoveryData;

use crate::config::blend::serde::Config;
use crate::config::deployment::{
    EraDefinition,
    parameters::{
        EraParameters,
        blend::{BlendParameters, v1 as blend_v1},
        cryptarchia::{CryptarchiaParameters, v1 as cryptarchia_v1},
        time::{TimeParameters, v1 as time_v1},
        v1,
    },
};

pub mod serde;

/// The settings of the Blend services in an era, on the libp2p backends.
type Libp2pBlendSettings = Settings<
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
        // The core swarm's transport is built once, so its receive window is
        // sized for the era that needs the most: a later era may carry larger
        // messages, or more of them.
        let receive_window = eras
            .iter()
            .map(|era| era_receive_window(&era.entry.parameters))
            .max()
            .expect("a chain has at least one era");
        let blend_settings =
            eras.map(|era| era_settings(&self.user, &era.entry.parameters, eras, receive_window));
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
/// whose eras are `eras`, for a node configured with `user`.
fn era_settings(
    user: &Config,
    era: &EraDefinition,
    eras: &EraSchedule<EraDefinition>,
    receive_window: u32,
) -> Libp2pBlendSettings {
    match &era.parameters {
        EraParameters::V1(v1::Parameters {
            blend: BlendParameters::V1(blend),
            cryptarchia: CryptarchiaParameters::V1(cryptarchia),
            time: TimeParameters::V1(time),
            ..
        }) => v1_settings(user, blend, cryptarchia, time, era, eras, receive_window),
    }
}

/// The receive window a core connection needs while `era` is in force.
fn era_receive_window(era: &EraDefinition) -> u32 {
    match &era.parameters {
        EraParameters::V1(v1::Parameters {
            blend: BlendParameters::V1(blend),
            ..
        }) => connection_receive_window(
            blend.connection_share_per_round(),
            blend.common.network_absorption_in_rounds,
            blend.common.num_blend_layers,
        ),
    }
}

/// The Blend settings from version 1 of the Blend section, with the slot and
/// epoch lengths its timing follows, from version 1 of the cryptarchia and time
/// sections.
#[expect(
    clippy::too_many_arguments,
    reason = "the sections and the eras they are of"
)]
fn v1_settings(
    user: &Config,
    blend: &blend_v1::Settings,
    cryptarchia: &cryptarchia_v1::Settings,
    time: &time_v1::Settings,
    era: &EraDefinition,
    eras: &EraSchedule<EraDefinition>,
    receive_window: u32,
) -> Libp2pBlendSettings {
    let slots_per_epoch = cryptarchia.slots_per_epoch();
    let slots_per_block = cryptarchia.average_slots_per_block();
    let slot_duration = time.slot_duration;
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
                receive_window,
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
    blend: &blend_v1::Settings,
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
