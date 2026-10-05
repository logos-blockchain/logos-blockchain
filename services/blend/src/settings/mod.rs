use ::core::num::NonZeroU64;
use serde::{Deserialize, Serialize};

use crate::{
    broadcast::settings::{
        EraSettings as BroadcastEraSettings, StartingBlendConfig as BroadcastConfig,
    },
    core::settings::{
        EraSettings as CoreEraSettings, SchedulerSettings, StartingBlendConfig as CoreConfig,
    },
    edge::settings::{EraSettings as EdgeEraSettings, StartingBlendConfig as EdgeConfig},
};

mod common;
pub use self::common::CommonSettings;
mod core;
pub use self::core::CoreSettings;
mod edge;
pub use self::edge::EdgeSettings;
mod timing;
pub use self::timing::TimingSettings;
pub mod user;

/// The settings of the Blend services in an era, in Blend's own versions:
/// the node builds them from the era's parameters, and each service takes its
/// part of them.
#[derive(Clone, Debug)]
pub enum EraSettings<CoreBackendSettings, EdgeBackendSettings, BroadcastSettings> {
    V1(Settings<CoreBackendSettings, EdgeBackendSettings, BroadcastSettings>),
}

impl<CoreBackendSettings, EdgeBackendSettings, BroadcastSettings>
    From<EraSettings<CoreBackendSettings, EdgeBackendSettings, BroadcastSettings>>
    for CoreEraSettings<CoreBackendSettings, BroadcastSettings>
{
    fn from(
        settings: EraSettings<CoreBackendSettings, EdgeBackendSettings, BroadcastSettings>,
    ) -> Self {
        match settings {
            EraSettings::V1(settings) => Self::V1(settings.into()),
        }
    }
}

impl<CoreBackendSettings, EdgeBackendSettings, BroadcastSettings>
    From<EraSettings<CoreBackendSettings, EdgeBackendSettings, BroadcastSettings>>
    for EdgeEraSettings<EdgeBackendSettings, BroadcastSettings>
{
    fn from(
        settings: EraSettings<CoreBackendSettings, EdgeBackendSettings, BroadcastSettings>,
    ) -> Self {
        match settings {
            EraSettings::V1(settings) => Self::V1(settings.into()),
        }
    }
}

impl<CoreBackendSettings, EdgeBackendSettings, BroadcastSettings>
    From<EraSettings<CoreBackendSettings, EdgeBackendSettings, BroadcastSettings>>
    for BroadcastEraSettings<BroadcastSettings>
{
    fn from(
        settings: EraSettings<CoreBackendSettings, EdgeBackendSettings, BroadcastSettings>,
    ) -> Self {
        match settings {
            EraSettings::V1(settings) => Self::V1(settings.into()),
        }
    }
}

/// The settings of the Blend services in an era of version 1.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Settings<CoreBackendSettings, EdgeBackendSettings, BroadcastSettings> {
    pub common: CommonSettings<BroadcastSettings>,
    pub core: CoreSettings<CoreBackendSettings>,
    pub edge: EdgeSettings<EdgeBackendSettings>,
}

impl<CoreBackendSettings, EdgeBackendSettings, BroadcastSettings>
    From<Settings<CoreBackendSettings, EdgeBackendSettings, BroadcastSettings>>
    for CoreConfig<CoreBackendSettings, BroadcastSettings>
{
    fn from(
        Settings {
            common:
                CommonSettings {
                    minimum_network_size,
                    time,
                    non_ephemeral_signing_key_id,
                    num_blend_layers,
                    data_replication_factor,
                    broadcast,
                    abstain_on_failure,
                },
            core:
                CoreSettings {
                    backend,
                    scheduler,
                    zk,
                    activity_threshold_sensitivity,
                },
            ..
        }: Settings<CoreBackendSettings, EdgeBackendSettings, BroadcastSettings>,
    ) -> Self {
        Self {
            backend,
            scheduler,
            time,
            zk,
            non_ephemeral_signing_key_id,
            num_blend_layers,
            minimum_network_size,
            data_replication_factor,
            activity_threshold_sensitivity,
            network: broadcast,
            abstain_on_failure,
        }
    }
}

impl<CoreBackendSettings, EdgeBackendSettings, BroadcastSettings>
    From<Settings<CoreBackendSettings, EdgeBackendSettings, BroadcastSettings>>
    for EdgeConfig<EdgeBackendSettings, BroadcastSettings>
{
    fn from(
        Settings {
            common:
                CommonSettings {
                    minimum_network_size,
                    time,
                    non_ephemeral_signing_key_id,
                    num_blend_layers,
                    data_replication_factor,
                    broadcast,
                    abstain_on_failure,
                },
            edge: EdgeSettings { backend },
            core:
                CoreSettings {
                    scheduler: SchedulerSettings { cover, delayer },
                    ..
                },
        }: Settings<CoreBackendSettings, EdgeBackendSettings, BroadcastSettings>,
    ) -> Self {
        Self {
            backend,
            time,
            non_ephemeral_signing_key_id,
            num_blend_layers,
            minimum_network_size,
            cover,
            data_replication_factor,
            network: broadcast,
            abstain_on_failure,
            // An edge node has no release schedule of its own, but the deadline it
            // waits out is the one a core node's schedule implies, so it takes the
            // same delay bound the core scheduler is configured with.
            max_blend_delay_in_rounds: delayer.maximum_release_delay_in_rounds,
        }
    }
}

impl<CoreBackendSettings, EdgeBackendSettings, NetworkSettings>
    From<Settings<CoreBackendSettings, EdgeBackendSettings, NetworkSettings>>
    for BroadcastConfig<NetworkSettings>
{
    fn from(
        Settings {
            common:
                CommonSettings {
                    minimum_network_size,
                    time,
                    non_ephemeral_signing_key_id,
                    broadcast,
                    ..
                },
            ..
        }: Settings<CoreBackendSettings, EdgeBackendSettings, NetworkSettings>,
    ) -> Self {
        Self {
            network: broadcast,
            time,
            non_ephemeral_signing_key_id,
            minimum_network_size,
        }
    }
}

/// `T_M`: the message traversal time, which is what a sender waits for its
/// payload to appear on the broadcasting channel before treating the message
/// carrying it as lost.
///
/// A message crosses `ß` blend nodes, each of which holds it for at most the
/// maximal blending delay `∆max`, and the network carries it for the absorption
/// `η` of one hop:
///
/// `T_M = ß · (∆max + η)`
#[must_use]
pub const fn max_data_message_delay_in_rounds(
    num_blend_layers: NonZeroU64,
    max_blend_delay_in_rounds: NonZeroU64,
    network_absorption_in_rounds: NonZeroU64,
) -> NonZeroU64 {
    match NonZeroU64::new(
        num_blend_layers.get().saturating_mul(
            max_blend_delay_in_rounds
                .get()
                .saturating_add(network_absorption_in_rounds.get()),
        ),
    ) {
        Some(delay) => delay,
        // Not `expect`, to keep this a `const fn`.
        None => panic!("Both factors of the delivery deadline are non-zero."),
    }
}
