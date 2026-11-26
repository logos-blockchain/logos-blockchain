use lb_cryptarchia_engine::time::EraSchedule;
use lb_pow_service::{PoWEraSettings, PoWServiceSettings};
use lb_services_utils::overwatch::RecoveryData;

use crate::config::{deployment::EraDefinition, pow::serde::Config};

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
        let (mining, start_on_boot) = self.user.mining.into_service_parts();
        PoWServiceSettings {
            mining,
            auto_claim: self.user.auto_claim.into(),
            start_on_boot,
            era_schedule: eras.map(|era| {
                let parameters = era.parameters.ruleset.parameters();
                let reward = &parameters.cryptarchia.pow_config.reward;
                PoWEraSettings {
                    slot_window: reward.slot_window,
                    rewards_enabled: reward.rate_num > 0,
                    max_claim_tx_size: usize::from(
                        era.parameters.ruleset.blend_payload_body_size(),
                    )
                    .min(parameters.cryptarchia.max_transactions_size as usize),
                    fork_digest: era.parameters.tx_fork_digest(),
                    gas_profile: parameters.cryptarchia.gas_profile,
                }
            }),
            recovery_data,
        }
    }
}
