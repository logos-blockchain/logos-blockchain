use core::{
    net::{IpAddr, Ipv4Addr},
    time::Duration,
};

use lb_utils::bounded_duration::{MinimalBoundedDuration, NANO};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use crate::config::utils;

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Config {
    #[serde(skip_serializing_if = "utils::is_default")]
    pub backend: NtpSettings,
}

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct NtpSettings {
    /// Ntp server address
    #[serde(skip_serializing_if = "is_default_server")]
    pub server: String,
    /// Ntp server settings
    #[serde(skip_serializing_if = "utils::is_default")]
    pub client: NtpClientSettings,
    /// Interval for the backend to contact the ntp server and update its time
    #[serde_as(as = "MinimalBoundedDuration<1, NANO>")]
    #[serde(skip_serializing_if = "is_default_update_interval")]
    pub update_interval: Duration,
}

fn default_server() -> String {
    "pool.ntp.org:123".to_owned()
}

fn is_default_server(server: &str) -> bool {
    server == default_server()
}

const fn default_update_interval() -> Duration {
    Duration::from_secs(15)
}

fn is_default_update_interval(update_interval: &Duration) -> bool {
    *update_interval == default_update_interval()
}

impl Default for NtpSettings {
    fn default() -> Self {
        Self {
            server: default_server(),
            client: NtpClientSettings::default(),
            update_interval: default_update_interval(),
        }
    }
}

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct NtpClientSettings {
    #[serde_as(as = "MinimalBoundedDuration<1, NANO>")]
    #[serde(skip_serializing_if = "is_default_timeout")]
    pub timeout: Duration,
    #[serde(skip_serializing_if = "is_default_listening_interface")]
    pub listening_interface: IpAddr,
}

const fn default_timeout() -> Duration {
    Duration::from_secs(5)
}

fn is_default_timeout(timeout: &Duration) -> bool {
    *timeout == default_timeout()
}

fn default_listening_interface() -> IpAddr {
    Ipv4Addr::UNSPECIFIED.into()
}

fn is_default_listening_interface(listening_interface: &IpAddr) -> bool {
    *listening_interface == default_listening_interface()
}

impl Default for NtpClientSettings {
    fn default() -> Self {
        Self {
            timeout: default_timeout(),
            listening_interface: default_listening_interface(),
        }
    }
}
