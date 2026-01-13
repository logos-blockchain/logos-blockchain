use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{
    core::settings::{BlendConfig as CoreConfig, SchedulerSettings},
    edge::settings::BlendConfig as EdgeConfig,
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
                    crypto,
                    minimum_network_size,
                    time,
                    recovery_path_prefix,
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
            crypto,
            scheduler,
            time,
            zk,
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
                    crypto,
                    minimum_network_size,
                    time,
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
            crypto,
            time,
            minimum_network_size,
            cover,
        }
    }
}
