use core::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
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
    pub listen_address: SocketAddr,
    /// Allowed origins for this server deployment requests.
    pub cors_origins: Vec<String>,
    /// Timeout for API requests in seconds.
    #[serde_as(as = "serde_with::DurationSeconds<u64>")]
    pub timeout: Duration,
    /// Maximum request body size in bytes.
    pub max_body_size: u64,
    /// Maximum number of concurrent requests.
    pub max_concurrent_requests: u64,
}

impl Default for AxumBackendSettings {
    fn default() -> Self {
        Self {
            listen_address: SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 8080).into(),
            cors_origins: Vec::default(),
            timeout: Duration::from_secs(30),
            max_body_size: 10 * 1024 * 1024,
            max_concurrent_requests: 500,
        }
    }
}
