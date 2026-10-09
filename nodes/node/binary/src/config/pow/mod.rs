use lb_cryptarchia_engine::era::EraSchedule;
use lb_pow_service::PoWServiceSettings;
use lb_services_utils::overwatch::RecoveryData;

use crate::config::{
    deployment::{EraDefinition, era::ruleset::EraRuleset},
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
        recovery_data: &RecoveryData,
        eras: &EraSchedule<EraDefinition>,
    ) -> EraSchedule<PoWServiceSettings> {
        eras.map(|era| {
            let user_config = self.user.clone();
            let EraRuleset::V1(parameters) = &era.entry.parameters.ruleset;
            let reward = &parameters.cryptarchia.pow_config.reward;
            PoWServiceSettings {
                mining: user_config.mining,
                auto_claim: user_config.auto_claim,
                slot_window: reward.slot_window,
                rewards_enabled: reward.rate_num > 0,
                // TODO: This will go once we update the PoW service to support era schedules.
                recovery_data: recovery_data.clone(),
            }
        })
    }
}
