use ::core::{num::NonZeroU64, time::Duration};
use serde::{Deserialize, Serialize};

use crate::{
    core::settings::{SchedulerSettings, StartingBlendConfig as CoreConfig},
    edge::settings::StartingBlendConfig as EdgeConfig,
};

mod common;
pub use self::common::CommonSettings;
mod core;
pub use self::core::CoreSettings;
mod edge;
pub use self::edge::EdgeSettings;
mod timing;
pub use self::timing::TimingSettings;

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
                    recovery_data,
                    non_ephemeral_signing_key_id,
                    num_blend_layers,
                    data_replication_factor,
                    broadcast,
                    blend_failure_fallback,
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
            recovery_data,
            data_replication_factor,
            activity_threshold_sensitivity,
            network: broadcast,
            blend_failure_fallback,
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
                    blend_failure_fallback,
                    ..
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
            blend_failure_fallback,
            // An edge node has no release schedule of its own, but the deadline it
            // waits out is the one a core node's schedule implies, so it takes the
            // same delay bound the core scheduler is configured with.
            max_blend_delay_in_rounds: delayer.maximum_release_delay_in_rounds,
        }
    }
}

#[must_use]
pub const fn round_duration_in_seconds(round_duration: Duration) -> NonZeroU64 {
    match NonZeroU64::new(round_duration.as_secs()) {
        Some(seconds) => seconds,
        None => panic!("Round duration must be at least one second."),
    }
}

#[must_use]
pub const fn max_data_message_delay_in_rounds(
    num_blend_layers: NonZeroU64,
    max_blend_delay_in_rounds: NonZeroU64,
    round_duration_in_seconds: NonZeroU64,
) -> NonZeroU64 {
    let dissemination_delay_in_rounds = Duration::from_secs(1)
        .as_secs()
        .div_ceil(round_duration_in_seconds.get());
    match NonZeroU64::new(
        num_blend_layers
            .get()
            .saturating_mul(
                max_blend_delay_in_rounds
                    .get()
                    .saturating_add(dissemination_delay_in_rounds),
            )
            .saturating_add(dissemination_delay_in_rounds),
    ) {
        Some(delay) => delay,
        // Not `expect`, to keep this a `const fn`.
        None => panic!("Both factors of the delivery deadline are non-zero."),
    }
}
