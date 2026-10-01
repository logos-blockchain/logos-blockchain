use lb_core::mantle::GenesisTime;
use lb_era_parameters::EraDefinition;
use lb_time_service::{
    TimeServiceSettings,
    backends::{NtpTimeBackendSettings, ntp::async_client::NTPClientSettings},
};

use crate::config::time::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    #[must_use]
    pub fn into_time_service_settings(
        self,
        era: &EraDefinition,
        genesis_time: GenesisTime,
    ) -> TimeServiceSettings<NtpTimeBackendSettings> {
        TimeServiceSettings::from_era(
            era,
            genesis_time.into(),
            NtpTimeBackendSettings {
                ntp_client_settings: NTPClientSettings {
                    timeout: self.user.backend.client.timeout,
                    listening_interface: self.user.backend.client.listening_interface,
                },
                ntp_server: self.user.backend.server,
                update_interval: self.user.backend.update_interval,
            },
        )
    }
}
