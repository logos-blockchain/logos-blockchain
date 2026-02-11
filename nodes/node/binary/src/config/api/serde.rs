use core::{net::SocketAddr, time::Duration};

use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub backend: AxumBackendSettings,
    #[cfg(feature = "testing")]
    pub testing: AxumBackendSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde_as]
pub struct AxumBackendSettings {
    pub address: SocketAddr,
    /// Allowed origins for this server deployment requests.
    pub cors_origins: Vec<String>,
    /// Timeout for API requests in seconds (default: 30)
    #[serde_as(as = "serde_with::DurationSeconds<u64>")]
    #[serde(default = "default_timeout")]
    pub timeout: Duration,
    /// Maximum request body size in bytes (default: 10MB)
    #[serde(default = "default_max_body_size")]
    pub max_body_size: usize,
    /// Maximum number of concurrent requests
    #[serde(default = "default_max_concurrent_requests")]
    pub max_concurrent_requests: usize,
}

const fn default_timeout() -> Duration {
    Duration::from_secs(30)
}

const fn default_max_body_size() -> usize {
    10 * 1024 * 1024
}

const fn default_max_concurrent_requests() -> usize {
    500
}
