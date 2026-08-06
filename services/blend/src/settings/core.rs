use serde::{Deserialize, Serialize};

use crate::core::settings::{SchedulerSettings, ZkSettings};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CoreSettings<BackendSettings, NetworkSettings> {
    pub backend: BackendSettings,
    pub network: NetworkSettings,
    pub scheduler: SchedulerSettings,
    pub zk: ZkSettings,
    pub activity_threshold_sensitivity: u64,
}
