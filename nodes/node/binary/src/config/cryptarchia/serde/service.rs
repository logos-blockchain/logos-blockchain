use core::time::Duration;

use lb_utils::bounded_duration::{MinimalBoundedDuration, SECOND};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use crate::config::utils;

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Config {
    #[serde(skip_serializing_if = "utils::is_default")]
    pub bootstrap: BootstrapConfig,
}

#[serde_as]
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct BootstrapConfig {
    #[serde_as(as = "MinimalBoundedDuration<0, SECOND>")]
    #[serde(skip_serializing_if = "is_default_prolonged_bootstrap_period")]
    pub prolonged_bootstrap_period: Duration,
    #[serde(skip_serializing_if = "utils::is_default")]
    pub force_bootstrap: bool,
    #[serde(skip_serializing_if = "utils::is_default")]
    pub offline_grace_period: OfflineGracePeriodConfig,
}

const fn default_prolonged_bootstrap_period() -> Duration {
    Duration::from_mins(5)
}

fn is_default_prolonged_bootstrap_period(value: &Duration) -> bool {
    *value == default_prolonged_bootstrap_period()
}

impl Default for BootstrapConfig {
    fn default() -> Self {
        Self {
            prolonged_bootstrap_period: default_prolonged_bootstrap_period(),
            force_bootstrap: bool::default(),
            offline_grace_period: OfflineGracePeriodConfig::default(),
        }
    }
}

#[serde_as]
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct OfflineGracePeriodConfig {
    /// Maximum duration a node can be offline before forcing bootstrap mode
    #[serde_as(as = "MinimalBoundedDuration<0, SECOND>")]
    #[serde(skip_serializing_if = "is_default_grace_period")]
    pub grace_period: Duration,
    /// Interval at which to record the current timestamp and engine state
    #[serde_as(as = "MinimalBoundedDuration<0, SECOND>")]
    #[serde(skip_serializing_if = "is_default_state_recording_interval")]
    pub state_recording_interval: Duration,
}

const fn default_grace_period() -> Duration {
    Duration::from_mins(20)
}

fn is_default_grace_period(value: &Duration) -> bool {
    *value == default_grace_period()
}

const fn default_state_recording_interval() -> Duration {
    Duration::from_secs(60)
}

fn is_default_state_recording_interval(value: &Duration) -> bool {
    *value == default_state_recording_interval()
}

impl Default for OfflineGracePeriodConfig {
    fn default() -> Self {
        Self {
            grace_period: default_grace_period(),
            state_recording_interval: default_state_recording_interval(),
        }
    }
}
