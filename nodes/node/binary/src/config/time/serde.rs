use core::{net::IpAddr, time::Duration};

use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    pub backend: NtpSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde_as]
pub struct NtpSettings {
    /// Ntp server address
    pub server: String,
    /// Ntp server settings
    pub client: NtpClientSettings,
    /// Interval for the backend to contact the ntp server and update its time
    #[serde_as(as = "MinimalBoundedDuration<1, NANO>")]
    pub update_interval: Duration,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde_as]
pub struct NtpClientSettings {
    #[serde_as(as = "MinimalBoundedDuration<1, NANO>")]
    pub timeout: Duration,
    pub listening_interface: IpAddr,
}
