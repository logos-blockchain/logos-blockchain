use std::{net::SocketAddr, time::Duration};

/// Configuration for the Http Server
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde_with::serde_as]
pub struct AxumBackendSettings {
    /// Socket where the server will be listening on for incoming requests.
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

impl Default for AxumBackendSettings {
    fn default() -> Self {
        Self {
            address: SocketAddr::from(([127, 0, 0, 1], 8080)),
            cors_origins: Vec::new(),
            timeout: default_timeout(),
            max_body_size: default_max_body_size(),
            max_concurrent_requests: default_max_concurrent_requests(),
        }
    }
}

const fn default_timeout() -> Duration {
    Duration::from_secs(30)
}

#[must_use]
pub const fn default_max_body_size() -> usize {
    // This has to allow for JSON object bytes, which is larger than the actual
    // payload data size. Typical JSON overhead for binary data:
    // - Base64 encoding: ~1.33× (33% increase).
    // - JSON structure (quotes, braces, commas): adds 10-20% typically.
    // - Combined realistic overhead: ~1.5-1.6× for Base64-encoded binary data in
    //   JSON.
    10 * 1024 * 1024
}

const fn default_max_concurrent_requests() -> usize {
    500
}
