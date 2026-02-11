use core::{net::IpAddr, time::Duration};

use lb_utils::bounded_duration::{MinimalBoundedDuration, NANO};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    pub backend: NtpSettings,
}

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NtpSettings {
    /// Ntp server address
    pub server: String,
    /// Ntp server settings
    pub client: NtpClientSettings,
    /// Interval for the backend to contact the ntp server and update its time
    #[serde_as(as = "MinimalBoundedDuration<1, NANO>")]
    pub update_interval: Duration,
}

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NtpClientSettings {
    #[serde_as(as = "MinimalBoundedDuration<1, NANO>")]
    pub timeout: Duration,
    pub listening_interface: IpAddr,
}
