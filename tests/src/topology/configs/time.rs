use std::{
    net::{IpAddr, Ipv4Addr},
    time::Duration,
};

use nomos_node::config::time::serde::Config;
use nomos_time::backends::{NtpTimeBackendSettings, ntp::async_client::NTPClientSettings};
use time::OffsetDateTime;

pub(crate) const DEFAULT_SLOT_TIME_IN_SECS: u64 = 2;
pub(crate) const CONSENSUS_SLOT_TIME_VAR: &str = "CONSENSUS_SLOT_TIME";

pub type GeneralTimeConfig = Config;

#[must_use]
pub fn default_time_config() -> GeneralTimeConfig {
    GeneralTimeConfig {
        backend: NtpTimeBackendSettings {
            ntp_client_settings: NTPClientSettings {
                timeout: Duration::from_secs(5),
                listening_interface: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            },
            ntp_server: "pool.ntp.org".to_owned(),
            update_interval: Duration::from_secs(16),
        },
        chain_start_time: OffsetDateTime::now_utc(),
    }
}
