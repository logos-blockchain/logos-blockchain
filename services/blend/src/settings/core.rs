use serde::{Deserialize, Serialize};

use crate::core::settings::{SchedulerSettings, ZkSettings};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CoreSettings<BackendSettings> {
    pub backend: BackendSettings,
    pub scheduler: SchedulerSettings,
    pub zk: ZkSettings,
}
