use std::time::Duration;

/// Configuration for gateway monitoring
#[derive(Debug, Clone, Copy)]
pub struct Settings {
    /// How often to check for gateway address changes
    pub check_interval: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_secs(300),
        }
    }
}
