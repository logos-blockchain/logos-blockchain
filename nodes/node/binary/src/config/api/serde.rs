use core::{
    net::{Ipv4Addr, SocketAddrV4},
    time::Duration,
};

use lb_utils::bounded_duration::{MinimalBoundedDuration, SECOND};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use crate::api::settings::{CorsOrigin, HostName};

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
    /// Browser origins allowed to use the API, such as `https://app.example`.
    /// A page from any other origin is refused.
    pub cors_origins: Vec<CorsOrigin>,
    /// Host names the API answers to, such as `node.example`, on top of
    /// `localhost` and any IP address. A request for any other name is refused,
    /// which is what stops a web page from reaching the API by DNS rebinding.
    pub allowed_hosts: Vec<HostName>,
    /// Timeout for API requests.
    #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
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
            allowed_hosts: Vec::default(),
            timeout: Duration::from_secs(30),
            max_body_size: 10 * 1024 * 1024,
            max_concurrent_requests: 500,
        }
    }
}
