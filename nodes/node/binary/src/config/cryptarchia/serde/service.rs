use core::time::Duration;

use lb_utils::bounded_duration::{MinimalBoundedDuration, SECOND};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub bootstrap: BootstrapConfig,
}

#[serde_as]
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct BootstrapConfig {
    #[serde_as(as = "MinimalBoundedDuration<0, SECOND>")]
    pub prolonged_bootstrap_period: Duration,
    pub force_bootstrap: bool,
    pub offline_grace_period: OfflineGracePeriodConfig,
}

impl Default for BootstrapConfig {
    fn default() -> Self {
        Self {
            prolonged_bootstrap_period: Duration::from_mins(5),
            force_bootstrap: bool::default(),
            offline_grace_period: OfflineGracePeriodConfig::default(),
        }
    }
}

#[serde_as]
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct OfflineGracePeriodConfig {
    /// Maximum duration a node can be offline before forcing bootstrap mode
    #[serde_as(as = "MinimalBoundedDuration<0, SECOND>")]
    pub grace_period: Duration,
    /// Interval at which to record the current timestamp and engine state
    #[serde_as(as = "MinimalBoundedDuration<0, SECOND>")]
    pub state_recording_interval: Duration,
}

impl Default for OfflineGracePeriodConfig {
    fn default() -> Self {
        Self {
            grace_period: Duration::from_mins(20),
            state_recording_interval: Duration::from_mins(1),
        }
    }
}
