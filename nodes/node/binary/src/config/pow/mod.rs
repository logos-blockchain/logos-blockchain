use lb_cryptarchia_engine::era::EraSchedule;
use lb_pow_service::PoWServiceSettings;
use lb_services_utils::overwatch::RecoveryData;

use crate::config::{
    deployment::{EraDefinition, era::parameters::EraParameters},
    pow::serde::Config,
};

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    #[must_use]
    pub fn into_pow_service_era_schedule(
        self,
        recovery_data: RecoveryData,
        eras: &EraSchedule<EraDefinition>,
    ) -> EraSchedule<PoWServiceSettings> {
        eras.map(|era| {
            let user_config = self.user.clone();
            let EraParameters::V1(parameters) = &era.entry.parameters.parameters;
            let reward = &parameters.cryptarchia.pow_config.reward;
            PoWServiceSettings {
                mining: user_config.mining,
                auto_claim: user_config.auto_claim,
                slot_window: reward.slot_window,
                rewards_enabled: reward.rate_num > 0,
                recovery_data: recovery_data.clone(),
            }
        })
    }
}
