use lb_time::era::EraSchedule;
use lb_time_service::{
    TimeServiceSettings,
    backends::{NtpTimeBackendSettings, ntp::async_client::NTPClientSettings},
};

use crate::config::{deployment::EraDefinition, time::serde::Config};

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    /// The settings of the time service: the user's NTP settings, and the eras
    /// of `eras`, which tell the service when each era starts and how long its
    /// slots and epochs last.
    #[must_use]
    pub fn into_time_service_settings(
        self,
        eras: &EraSchedule<EraDefinition>,
    ) -> TimeServiceSettings<NtpTimeBackendSettings> {
        TimeServiceSettings {
            backend: NtpTimeBackendSettings {
                ntp_client_settings: NTPClientSettings {
                    timeout: self.user.backend.client.timeout,
                    listening_interface: self.user.backend.client.listening_interface,
                },
                ntp_server: self.user.backend.server,
                update_interval: self.user.backend.update_interval,
            },
            era_schedule: eras.map(|_| ()),
        }
    }
}
