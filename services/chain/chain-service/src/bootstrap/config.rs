use std::time::Duration;

#[derive(Debug, Clone)]
pub struct BootstrapConfig {
    pub prolonged_bootstrap_period: Duration,
    pub force_bootstrap: bool,
    pub offline_grace_period: OfflineGracePeriodConfig,
}

#[derive(Debug, Clone)]
pub struct OfflineGracePeriodConfig {
    /// Maximum duration a node can be offline before forcing bootstrap mode
    pub grace_period: Duration,
    /// Interval at which to record the current timestamp and engine state
    pub state_recording_interval: Duration,
}

const fn default_offline_grace_period() -> Duration {
    Duration::from_secs(20 * 60)
}

const fn default_state_recording_interval() -> Duration {
    Duration::from_secs(60)
}

impl Default for OfflineGracePeriodConfig {
    fn default() -> Self {
        Self {
            grace_period: default_offline_grace_period(),
            state_recording_interval: default_state_recording_interval(),
        }
    }
}
