use lb_pow_service::PoWServiceSettings;
use lb_services_utils::overwatch::RecoveryData;

use crate::config::pow::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    #[must_use]
    pub const fn into_pow_service_settings(
        self,
        recovery_data: RecoveryData,
    ) -> PoWServiceSettings {
        PoWServiceSettings {
            claim_address: self.user.claim_address,
            mining: self.user.mining,
            recovery_data,
        }
    }
}
