use serde::{Deserialize, Serialize};

use crate::core::settings::{SchedulerSettings, ZkSettings};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CoreSettings<BackendSettings, NetworkSettings> {
    pub backend: BackendSettings,
    /// Where a message arriving off the Blend network is republished.
    pub network: NetworkSettings,
    pub scheduler: SchedulerSettings,
    pub zk: ZkSettings,
    pub activity_threshold_sensitivity: u64,
}
