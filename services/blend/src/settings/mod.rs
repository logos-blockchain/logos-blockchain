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
            broadcast,
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
                    // An edge node keeps nothing across a restart, so the recovery
                    // state this carries is not one of the things it takes.
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
            broadcast,
            blend_failure_fallback,
            // An edge node has no release schedule of its own, but the deadline it
            // waits out is the one a core node's schedule implies, so it takes the
            // same delay bound the core scheduler is configured with.
            max_blend_delay_in_rounds: delayer.maximum_release_delay_in_rounds,
        }
    }
}
