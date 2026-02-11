use std::{
    net::{IpAddr, Ipv4Addr},
    time::Duration,
};

use lb_node::config::time::serde as time;

pub(crate) const DEFAULT_SLOT_TIME_IN_SECS: u64 = 1;
pub(crate) const CONSENSUS_SLOT_TIME_VAR: &str = "CONSENSUS_SLOT_TIME";

pub type GeneralTimeConfig = time::Config;

#[must_use]
pub fn default_time_config() -> GeneralTimeConfig {
    GeneralTimeConfig {
        backend: time::NtpSettings {
            client: time::NtpClientSettings {
                timeout: Duration::from_secs(5),
                listening_interface: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            },
            server: "pool.ntp.org:123".to_owned(),
            update_interval: Duration::from_secs(16),
        },
    }
}
