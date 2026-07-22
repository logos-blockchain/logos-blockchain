use core::{
    net::{Ipv4Addr, SocketAddrV4},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub backend: AxumBackendSettings,
}

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AxumBackendSettings {
    /// Listening address.
    pub listen_address: core::net::SocketAddr,
    /// Allowed origins for these server deployment requests.
    pub cors_origins: Vec<String>,
    /// Timeout for API requests in seconds.
    #[serde_as(as = "serde_with::DurationSeconds<u64>")]
    pub timeout: Duration,
    /// Maximum request body size in bytes.
    pub max_body_size: u64,
    /// Maximum number of concurrent requests.
    pub max_concurrent_requests: u64,
}

impl AxumBackendSettings {
    #[must_use]
    pub const fn default_port() -> u16 {
        8080
    }

    #[must_use]
    pub fn default_listening_address(port: u16) -> core::net::SocketAddr {
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into()
    }
}

impl Default for AxumBackendSettings {
    fn default() -> Self {
        Self {
            listen_address: Self::default_listening_address(Self::default_port()),
            cors_origins: Vec::default(),
            timeout: Duration::from_secs(30),
            max_body_size: 10 * 1024 * 1024,
            max_concurrent_requests: 500,
        }
    }
}
