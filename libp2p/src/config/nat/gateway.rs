use std::time::Duration;

/// Configuration for gateway monitoring
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    /// How often to check for gateway address changes
    #[serde(default = "default_check_interval")]
    pub check_interval: Duration,
}

const fn default_check_interval() -> Duration {
    Duration::from_secs(300)
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            check_interval: default_check_interval(),
        }
    }
}
