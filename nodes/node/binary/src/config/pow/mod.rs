use lb_era_parameters::EraDefinition;
use lb_pow_service::PoWServiceSettings;
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
        era: &EraDefinition,
        recovery_data: RecoveryData,
    ) -> PoWServiceSettings {
        PoWServiceSettings::from_era(era, self.user.mining, self.user.auto_claim, recovery_data)
    }
}
