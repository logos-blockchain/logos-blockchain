use crate::core::settings::{SchedulerSettings, ZkSettings};

#[derive(Clone, Debug)]
pub struct CoreSettings<BackendSettings> {
    pub backend: BackendSettings,
    pub scheduler: SchedulerSettings,
    pub zk: ZkSettings,
    pub activity_threshold_sensitivity: u64,
}
