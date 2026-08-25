use lb_sdp_service::{ActiveMessageTrackerConfig, SdpSettings, wallet::SdpWalletConfig};
use lb_services_utils::overwatch::RecoveryData;

use crate::config::sdp::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    #[must_use]
    pub const fn into_sdp_service_settings(self, recovery_data: RecoveryData) -> SdpSettings {
        SdpSettings {
            declaration_id: self.user.declaration_id,
            wallet_config: SdpWalletConfig {
                funding_pk: self.user.wallet.funding_pk,
                max_tx_fee: self.user.wallet.max_tx_fee,
            },
            active_message_tracker: ActiveMessageTrackerConfig {
                status_check_interval_in_tip_changes: self
                    .user
                    .active_message_tracker
                    .status_check_interval_in_tip_changes,
                max_status_checks: self.user.active_message_tracker.max_status_checks,
            },
            recovery_data,
        }
    }
}
