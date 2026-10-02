use std::sync::Arc;

use ::core::time::Duration;
use lb_cryptarchia_engine::era::Eras;
use lb_era_parameters::{EraDefinition, EraParameters, v1::blend::Settings as BlendParameters};

use crate::{
    broadcast::settings::StartingBlendConfig as BroadcastConfig,
    core::{
        backends::libp2p::Libp2pBlendBackendSettings as Libp2pCoreBackendSettings,
        dispatcher::libp2p::Libp2pBroadcastSettings,
        settings::{
            CoverTrafficSettings, MessageDelayerSettings, SchedulerSettings,
            StartingBlendConfig as CoreConfig, ZkSettings,
        },
    },
    edge::{
        backends::libp2p::Libp2pBlendBackendSettings as Libp2pEdgeBackendSettings,
        settings::StartingBlendConfig as EdgeConfig,
    },
    settings::{CommonSettings, CoreSettings, EdgeSettings, Settings, TimingSettings, user},
};

/// What the node hands a Blend service: the Blend section of its
/// configuration and the chain's eras, from which the service builds its
/// settings for every era when it starts.
#[derive(Clone, Debug)]
pub struct ServiceSettings {
    pub user: user::Config,
    pub eras: Arc<Eras<EraDefinition>>,
}

impl ServiceSettings {
    /// The service's settings in every era of the chain.
    #[must_use]
    pub fn in_every_era<EraSettings: FromEra>(&self) -> Eras<EraSettings> {
        self.eras
            .map(|era| EraSettings::from_era(&self.user, &era.entry.parameters, &self.eras))
    }
}

/// Settings a Blend service runs an era under, built from the node's
/// configuration, the era, and the chain's other eras.
pub trait FromEra {
    fn from_era(user: &user::Config, era: &EraDefinition, eras: &Eras<EraDefinition>) -> Self;
}

/// The settings of the Blend services, on the libp2p backends.
type Libp2pSettings =
    Settings<Libp2pCoreBackendSettings, Libp2pEdgeBackendSettings, Libp2pBroadcastSettings>;

impl FromEra for Libp2pSettings {
    fn from_era(user: &user::Config, era: &EraDefinition, eras: &Eras<EraDefinition>) -> Self {
        let EraParameters::V1(parameters) = &era.parameters;
        let blend = &parameters.blend;
        let slots_per_epoch = parameters.cryptarchia.slots_per_epoch();
        let slots_per_block = parameters.cryptarchia.average_slots_per_block();
        let slot_duration = parameters.time.slot_duration;
        let protocol_name = era.protocol_names.blend.clone();

        Self {
            common: CommonSettings {
                non_ephemeral_signing_key_id: user.non_ephemeral_signing_key_id.clone(),
                num_blend_layers: blend.common.num_blend_layers,
                minimum_network_size: blend.common.minimum_network_size.into(),
                // A proposal goes out on the topic of its own era, which is
                // not always the era in force.
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
                backend: Libp2pCoreBackendSettings {
                    target_peering_degree: blend.core.target_peering_degree,
                    connection_share_per_round: blend.connection_share_per_round(),
                    listening_address: user.core.backend.listening_address.clone(),
                    edge_node_connection_timeout: blend
                        .edge_node_connection_timeout(&slot_duration),
                    max_dial_attempts_per_peer: user.core.backend.max_dial_attempts_per_peer,
                    max_edge_node_incoming_connections: blend.maximum_concurrent_edge_connections(),
                    accepted_edge_connections_per_round: blend
                        .accepted_edge_connections_per_round(),
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
                backend: Libp2pEdgeBackendSettings {
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
}

impl FromEra for CoreConfig<Libp2pCoreBackendSettings, Libp2pBroadcastSettings> {
    fn from_era(user: &user::Config, era: &EraDefinition, eras: &Eras<EraDefinition>) -> Self {
        Libp2pSettings::from_era(user, era, eras).into()
    }
}

impl FromEra for EdgeConfig<Libp2pEdgeBackendSettings, Libp2pBroadcastSettings> {
    fn from_era(user: &user::Config, era: &EraDefinition, eras: &Eras<EraDefinition>) -> Self {
        Libp2pSettings::from_era(user, era, eras).into()
    }
}

impl FromEra for BroadcastConfig<Libp2pBroadcastSettings> {
    fn from_era(user: &user::Config, era: &EraDefinition, eras: &Eras<EraDefinition>) -> Self {
        Libp2pSettings::from_era(user, era, eras).into()
    }
}

fn timing_settings(
    blend: &BlendParameters,
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
