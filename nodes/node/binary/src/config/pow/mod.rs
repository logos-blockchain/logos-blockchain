use core::num::NonZeroU64;

use lb_pow_service::PoWServiceSettings;
use lb_services_utils::overwatch::RecoveryData;

use crate::config::pow::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    /// `slot_window` is the consensus acceptance window and `rewards_enabled`
    /// whether the consensus distribution rate is non-zero, both sourced from
    /// the cryptarchia deployment configuration so the mining service and the
    /// ledger agree on a single value.
    #[must_use]
    pub fn into_pow_service_settings(
        self,
        recovery_data: RecoveryData,
        slot_window: NonZeroU64,
        rewards_enabled: bool,
    ) -> PoWServiceSettings {
        PoWServiceSettings {
            mining: self.user.mining,
            auto_claim: self.user.auto_claim,
            slot_window,
            rewards_enabled,
            recovery_data,
        }
    }
}
