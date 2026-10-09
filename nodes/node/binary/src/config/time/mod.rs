use lb_core::mantle::GenesisTime;
use lb_cryptarchia_engine::{
    EpochConfig,
    time::{EraSchedule, SlotConfig},
};
use lb_time_service::{
    TimeServiceSettings,
    backends::{NtpTimeBackendSettings, ntp::async_client::NTPClientSettings},
};

use crate::config::{
    deployment::{EraDefinition, era::ruleset::EraRuleset},
    time::serde::Config,
};

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    /// The settings of the time service in every era of `eras`. Their slot
    /// clocks count from genesis, so they only hold for the genesis era: the
    /// service has to read the whole schedule's timing to follow more than
    /// one era.
    #[must_use]
    pub fn into_time_service_era_schedule(
        self,
        eras: &EraSchedule<EraDefinition>,
        genesis_time: GenesisTime,
    ) -> EraSchedule<TimeServiceSettings<NtpTimeBackendSettings>> {
        eras.map(|era| {
            let user_config = self.user.clone();
            let EraRuleset::V1(parameters) = &era.entry.parameters.ruleset;
            let (deployment, cryptarchia_deployment) = (&parameters.time, &parameters.cryptarchia);
            TimeServiceSettings {
                slot_config: SlotConfig {
                    slot_duration: deployment.slot_duration,
                    genesis_time: genesis_time.into(),
                },
                epoch_config: EpochConfig {
                    epoch_period_nonce_buffer: cryptarchia_deployment
                        .epoch_config
                        .epoch_period_nonce_buffer,
                    epoch_stake_distribution_stabilization: cryptarchia_deployment
                        .epoch_config
                        .epoch_stake_distribution_stabilization,
                    epoch_period_nonce_stabilization: cryptarchia_deployment
                        .epoch_config
                        .epoch_period_nonce_stabilization,
                },
                base_period_length: cryptarchia_deployment
                    .consensus_config()
                    .base_period_length(),
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
