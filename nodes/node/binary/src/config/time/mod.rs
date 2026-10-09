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
    /// The settings of the time service in every era of `eras`. When each era
    /// starts, and how long its slots and epochs last, the schedule itself
    /// tells the service.
    #[must_use]
    pub fn into_time_service_era_schedule(
        self,
        eras: &EraSchedule<EraDefinition>,
    ) -> EraSchedule<TimeServiceSettings<NtpTimeBackendSettings>> {
        eras.map(|_| {
            let user_config = self.user.clone();
            TimeServiceSettings {
                backend: NtpTimeBackendSettings {
                    ntp_client_settings: NTPClientSettings {
                        timeout: user_config.backend.client.timeout,
                        listening_interface: user_config.backend.client.listening_interface,
                    },
                    ntp_server: user_config.backend.server,
                    update_interval: user_config.backend.update_interval,
                },
            }
        })
    }
}
