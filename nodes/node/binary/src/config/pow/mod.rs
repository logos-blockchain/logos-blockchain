use lb_cryptarchia_engine::era::EraSchedule;
use lb_era_parameters::{EraDefinition, EraParameters};
use lb_pow_service::{EraSettings, PoWServiceSettings};
use lb_services_utils::overwatch::RecoveryData;

use crate::config::pow::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    #[must_use]
    pub fn into_pow_service_settings(
        self,
        eras: &EraSchedule<EraDefinition>,
        recovery_data: RecoveryData,
    ) -> PoWServiceSettings {
        PoWServiceSettings {
            mining: self.user.mining,
            auto_claim: self.user.auto_claim,
            eras: eras.map(|era| era_settings(&era.entry.parameters.parameters)),
            recovery_data,
        }
    }
}

/// What the `PoW` service follows of an era, in the service's own versions.
const fn era_settings(parameters: &EraParameters) -> EraSettings {
    match parameters {
        EraParameters::V1(parameters) => {
            let reward = &parameters.cryptarchia.pow_config.reward;
            EraSettings::V1 {
                slot_window: reward.slot_window,
                rewards_enabled: reward.rate_num > 0,
            }
        }
    }
}
