use lb_time::era::{EpochLength as _, EraSchedule, EraTiming, SlotDuration as _};
use lb_time_service::{
    TimeServiceEraSettings, TimeServiceSettings,
    backends::{NtpTimeBackendSettings, ntp::async_client::NTPClientSettings},
};

use crate::config::{deployment::EraDefinition, time::serde::Config};

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    /// The settings of the time service: the user's NTP settings, and the eras
    /// of `eras`, which tell the service when each era starts, how long its
    /// slots and epochs last, and for how many of its slots the network keeps
    /// serving its protocols once the next era starts.
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
            era_schedule: eras.map(|era| era_settings(era.parameters)),
        }
    }
}

/// How the time service follows `era`.
fn era_settings(era: &EraDefinition) -> TimeServiceEraSettings {
    TimeServiceEraSettings {
        timing: EraTiming {
            slot_duration: era.slot_duration(),
            epoch_length: era.epoch_length(),
        },
        transition_slots: era.ruleset.era_transition_slots(),
    }
}

/// Checks that the transition period each era opens ends before the era after
/// it starts: the time service follows one transition period at a time.
///
/// # Panics
///
/// If an era ends within the transition period it opens.
pub(crate) fn check_transition_periods(eras: &EraSchedule<EraDefinition>) {
    for ((previous, era), next) in eras
        .iter()
        .zip(eras.iter().skip(1))
        .zip(eras.iter().skip(2))
    {
        let era_slots = next.first_slot.into_inner() - era.first_slot.into_inner();
        let transition_slots = era_settings(previous.parameters)
            .transition_period_in_slots_of(&era_settings(era.parameters))
            .unwrap_or(u64::MAX);
        assert!(
            transition_slots < era_slots,
            "Era {} lasts {era_slots} slots, not more than the {transition_slots} slots of the \
             transition period it opens.",
            era.era.into_inner()
        );
    }
}
