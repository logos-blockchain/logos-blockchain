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
pub struct Settings<CoreBackendSettings, EdgeBackendSettings, NetworkSettings> {
    pub common: CommonSettings,
    pub core: CoreSettings<CoreBackendSettings, NetworkSettings>,
    pub edge: EdgeSettings<EdgeBackendSettings>,
}

impl<CoreBackendSettings, EdgeBackendSettings, NetworkSettings>
    From<Settings<CoreBackendSettings, EdgeBackendSettings, NetworkSettings>>
    for CoreConfig<CoreBackendSettings, NetworkSettings>
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
                },
            core:
                CoreSettings {
                    backend,
                    network,
                    scheduler,
                    zk,
                    activity_threshold_sensitivity,
                },
            ..
        }: Settings<CoreBackendSettings, EdgeBackendSettings, NetworkSettings>,
    ) -> Self {
        Self {
            backend,
            network,
            scheduler,
            time,
            zk,
            non_ephemeral_signing_key_id,
            num_blend_layers,
            minimum_network_size,
            recovery_data,
            data_replication_factor,
            activity_threshold_sensitivity,
        }
    }
}

impl<CoreBackendSettings, EdgeBackendSettings, NetworkSettings>
    From<Settings<CoreBackendSettings, EdgeBackendSettings, NetworkSettings>>
    for EdgeConfig<EdgeBackendSettings>
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
                    ..
                },
            edge: EdgeSettings { backend },
            core:
                CoreSettings {
                    scheduler: SchedulerSettings { cover, .. },
                    ..
                },
        }: Settings<CoreBackendSettings, EdgeBackendSettings, NetworkSettings>,
    ) -> Self {
        Self {
            backend,
            time,
            non_ephemeral_signing_key_id,
            num_blend_layers,
            minimum_network_size,
            cover,
            data_replication_factor,
        }
    }
}
