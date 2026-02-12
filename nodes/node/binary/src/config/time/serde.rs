use core::{
    net::{IpAddr, Ipv4Addr},
    time::Duration,
};

use lb_utils::bounded_duration::{MinimalBoundedDuration, NANO};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct Config {
    pub backend: NtpSettings,
}

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct NtpSettings {
    /// Ntp server address
    pub server: String,
    /// Ntp server settings
    pub client: NtpClientSettings,
    /// Interval for the backend to contact the ntp server and update its time
    #[serde_as(as = "MinimalBoundedDuration<1, NANO>")]
    pub update_interval: Duration,
}

impl Default for NtpSettings {
    fn default() -> Self {
        Self {
            server: "pool.ntp.org:123".to_owned(),
            client: NtpClientSettings::default(),
            update_interval: Duration::from_secs(15),
        }
    }
}

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct NtpClientSettings {
    #[serde_as(as = "MinimalBoundedDuration<1, NANO>")]
    pub timeout: Duration,
    pub listening_interface: IpAddr,
}

impl Default for NtpClientSettings {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
            listening_interface: Ipv4Addr::UNSPECIFIED.into(),
        }
    }
}
