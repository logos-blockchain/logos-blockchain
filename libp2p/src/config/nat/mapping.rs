use std::time::Duration;

use lb_utils::math::PositiveF64;

#[derive(Debug, Clone, Copy)]
pub struct Settings {
    pub timeout: Duration,
    pub lease_duration: Duration,
    pub max_retries: u32,
    pub renewal_delay_fraction: PositiveF64,
    pub retry_interval: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(1),
            lease_duration: Duration::from_hours(2),
            max_retries: 3,
            renewal_delay_fraction: PositiveF64::try_from(0.8).expect("0.8 is positive"),
            retry_interval: Duration::from_secs(30),
        }
    }
}
