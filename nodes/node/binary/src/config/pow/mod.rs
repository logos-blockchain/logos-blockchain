use lb_pow_service::PoWServiceSettings;
use lb_services_utils::overwatch::RecoveryData;

use crate::config::pow::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    /// `slot_window` is the consensus acceptance window, sourced from the
    /// cryptarchia deployment configuration so the mining service and the
    /// ledger agree on a single value.
    #[must_use]
    pub const fn into_pow_service_settings(
        self,
        recovery_data: RecoveryData,
        slot_window: u64,
    ) -> PoWServiceSettings {
        PoWServiceSettings {
            claim_address: self.user.claim_address,
            mining: self.user.mining,
            slot_window,
            recovery_data,
        }
    }
}
