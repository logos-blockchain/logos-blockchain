use std::time::Duration;

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

pub(crate) const FIRST_STREAM_ITEM_READY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Settings<CoreBackendSettings, EdgeBackendSettings> {
    pub common: CommonSettings,
    pub core: CoreSettings<CoreBackendSettings>,
    pub edge: EdgeSettings<EdgeBackendSettings>,
}

impl<CoreBackendSettings, EdgeBackendSettings>
    From<Settings<CoreBackendSettings, EdgeBackendSettings>> for CoreConfig<CoreBackendSettings>
{
    fn from(
        Settings {
            common:
                CommonSettings {
                    minimum_network_size,
                    time,
                    recovery_path_prefix,
                    non_ephemeral_signing_key_id,
                    num_blend_layers,
                },
            core:
                CoreSettings {
                    backend,
                    scheduler,
                    zk,
                },
            ..
        }: Settings<CoreBackendSettings, EdgeBackendSettings>,
    ) -> Self {
        let recovery_path = {
            let mut path = recovery_path_prefix.join("core");
            path.set_extension("json");
            path
        };
        Self {
            backend,
            scheduler,
            time,
            zk,
            non_ephemeral_signing_key_id,
            num_blend_layers,
            minimum_network_size,
            recovery_path,
        }
    }
}

impl<CoreBackendSettings, EdgeBackendSettings>
    From<Settings<CoreBackendSettings, EdgeBackendSettings>> for EdgeConfig<EdgeBackendSettings>
{
    fn from(
        Settings {
            common:
                CommonSettings {
                    minimum_network_size,
                    time,
                    non_ephemeral_signing_key_id,
                    num_blend_layers,
                    ..
                },
            edge: EdgeSettings { backend },
            core:
                CoreSettings {
                    scheduler: SchedulerSettings { cover, .. },
                    ..
                },
        }: Settings<CoreBackendSettings, EdgeBackendSettings>,
    ) -> Self {
        Self {
            backend,
            time,
            non_ephemeral_signing_key_id,
            num_blend_layers,
            minimum_network_size,
            cover,
        }
    }
}
