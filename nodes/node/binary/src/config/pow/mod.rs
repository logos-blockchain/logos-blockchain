use lb_cryptarchia_engine::time::EraSchedule;
use lb_pow_service::{PoWEraSettings, PoWServiceSettings};
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
    pub fn into_pow_service_settings(
        self,
        recovery_data: RecoveryData,
        eras: &EraSchedule<EraDefinition>,
    ) -> PoWServiceSettings {
        PoWServiceSettings {
            mining: self.user.mining,
            auto_claim: self.user.auto_claim,
            era_schedule: eras.map(|era| {
                let EraRuleset::V1(parameters) = &era.parameters.ruleset;
                let reward = &parameters.cryptarchia.pow_config.reward;
                PoWEraSettings {
                    slot_window: reward.slot_window,
                    rewards_enabled: reward.rate_num > 0,
                }
            }),
            recovery_data,
        }
    }
}
