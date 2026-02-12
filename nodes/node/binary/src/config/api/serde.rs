use core::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub backend: AxumBackendSettings,
    #[cfg(feature = "testing")]
    pub testing: AxumBackendSettings,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            backend: AxumBackendSettings::default(),
            #[cfg(feature = "testing")]
            testing: AxumBackendSettings {
                listen_address: SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 8081).into(),
                ..AxumBackendSettings::default()
            },
        }
    }
}

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AxumBackendSettings {
    /// Listening address.
    #[serde(default = "default_listen_address")]
    pub listen_address: SocketAddr,
    /// Allowed origins for this server deployment requests.
    #[serde(default)]
    pub cors_origins: Vec<String>,
    /// Timeout for API requests in seconds.
    #[serde_as(as = "serde_with::DurationSeconds<u64>")]
    #[serde(default = "default_timeout")]
    pub timeout: Duration,
    /// Maximum request body size in bytes.
    #[serde(default = "default_max_body_size")]
    pub max_body_size: u64,
    /// Maximum number of concurrent requests.
    #[serde(default = "default_max_concurrent_requests")]
    pub max_concurrent_requests: u64,
}

const fn default_listen_address() -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 8080))
}

const fn default_timeout() -> Duration {
    Duration::from_secs(30)
}

const fn default_max_body_size() -> u64 {
    10 * 1024 * 1024
}

const fn default_max_concurrent_requests() -> u64 {
    500
}

impl Default for AxumBackendSettings {
    fn default() -> Self {
        Self {
            listen_address: default_listen_address(),
            cors_origins: Vec::default(),
            timeout: default_timeout(),
            max_body_size: default_max_body_size(),
            max_concurrent_requests: default_max_concurrent_requests(),
        }
    }
}
