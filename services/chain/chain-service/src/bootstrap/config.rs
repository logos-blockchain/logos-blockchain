use std::time::Duration;

use lb_utils::bounded_duration::{MinimalBoundedDuration, SECOND};
use serde::{Deserialize, Serialize};

#[serde_with::serde_as]
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BootstrapConfig {
    #[serde_as(as = "MinimalBoundedDuration<0, SECOND>")]
    pub prolonged_bootstrap_period: Duration,
    pub force_bootstrap: bool,
    #[serde(default)]
    pub offline_grace_period: OfflineGracePeriodConfig,
}

#[serde_with::serde_as]
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OfflineGracePeriodConfig {
    /// Maximum duration a node can be offline before forcing bootstrap mode
    #[serde_as(as = "MinimalBoundedDuration<0, SECOND>")]
    #[serde(default = "default_offline_grace_period")]
    pub grace_period: Duration,
    /// Interval at which to record the current timestamp and engine state
    #[serde_as(as = "MinimalBoundedDuration<0, SECOND>")]
    #[serde(default = "default_state_recording_interval")]
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
