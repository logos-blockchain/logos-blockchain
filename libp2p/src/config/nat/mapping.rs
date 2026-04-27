use std::time::Duration;

use lb_utils::{
    bounded_duration::{MinimalBoundedDuration, SECOND},
    math::PositiveF64,
};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

const DEFAULT_RENEWAL_DELAY_FRACTION: f64 = 0.8;

#[serde_as]
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_timeout")]
    #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
    pub timeout: Duration,
    #[serde(default = "default_lifetime")]
    pub lease_duration: Duration,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default = "default_renewal_delay_fraction")]
    pub renewal_delay_fraction: PositiveF64,
    #[serde(default = "default_retry_interval")]
    pub retry_interval: Duration,
}

const fn default_timeout() -> Duration {
    Duration::from_secs(1)
}

const fn default_lifetime() -> Duration {
    Duration::from_hours(2)
}

const fn default_max_retries() -> u32 {
    3
}

fn default_renewal_delay_fraction() -> PositiveF64 {
    PositiveF64::try_from(DEFAULT_RENEWAL_DELAY_FRACTION).expect("0.8 is positive")
}

const fn default_retry_interval() -> Duration {
    Duration::from_secs(30)
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            timeout: default_timeout(),
            lease_duration: default_lifetime(),
            max_retries: default_max_retries(),
            renewal_delay_fraction: default_renewal_delay_fraction(),
            retry_interval: default_retry_interval(),
        }
    }
}
