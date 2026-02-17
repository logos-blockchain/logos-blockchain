#![allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "Serde conditional serialization skip requires a specific function signature."
)]

use core::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    time::Duration,
};
#[cfg(feature = "testing")]
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use crate::config::utils;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Config {
    #[serde(skip_serializing_if = "utils::is_default")]
    pub backend: AxumBackendSettings,
    #[cfg(feature = "testing")]
    #[serde(skip_serializing_if = "is_testing_default")]
    pub testing: AxumBackendSettings,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            backend: AxumBackendSettings::default(),
            #[cfg(feature = "testing")]
            testing: DEFAULT_TESTING_CONFIG.clone(),
        }
    }
}

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AxumBackendSettings {
    /// Listening address.
    pub listen_address: SocketAddr,
    /// Allowed origins for this server deployment requests.
    #[serde(skip_serializing_if = "utils::is_default")]
    pub cors_origins: Vec<String>,
    /// Timeout for API requests in seconds.
    #[serde_as(as = "serde_with::DurationSeconds<u64>")]
    #[serde(skip_serializing_if = "is_default_timeout")]
    pub timeout: Duration,
    /// Maximum request body size in bytes.
    #[serde(skip_serializing_if = "is_default_max_body_size")]
    pub max_body_size: u64,
    /// Maximum number of concurrent requests.
    #[serde(skip_serializing_if = "is_default_max_concurrent_requests")]
    pub max_concurrent_requests: u64,
}

const fn default_timeout() -> Duration {
    Duration::from_secs(30)
}

fn is_default_timeout(timeout: &Duration) -> bool {
    *timeout == default_timeout()
}

const fn default_max_body_size() -> u64 {
    10 * 1024 * 1024
}

const fn is_default_max_body_size(max_body_size: &u64) -> bool {
    *max_body_size == default_max_body_size()
}

const fn default_max_concurrent_requests() -> u64 {
    500
}

const fn is_default_max_concurrent_requests(max_concurrent_requests: &u64) -> bool {
    *max_concurrent_requests == default_max_concurrent_requests()
}

impl Default for AxumBackendSettings {
    fn default() -> Self {
        Self {
            listen_address: SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 8080).into(),
            cors_origins: Vec::default(),
            timeout: default_timeout(),
            max_body_size: default_max_body_size(),
            max_concurrent_requests: default_max_concurrent_requests(),
        }
    }
}

#[cfg(feature = "testing")]
static DEFAULT_TESTING_CONFIG: LazyLock<AxumBackendSettings> = LazyLock::new(|| {
    let mut self_instance = AxumBackendSettings::default();
    self_instance.listen_address.set_port(8081);
    self_instance
});

#[cfg(feature = "testing")]
fn is_testing_default(settings: &AxumBackendSettings) -> bool {
    settings == &*DEFAULT_TESTING_CONFIG
}
