use lb_sdp_service::{ActiveMessageTrackerConfig, SdpSettings, wallet::SdpWalletConfig};
use lb_services_utils::overwatch::RecoveryData;

use crate::config::sdp::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    #[must_use]
    pub fn into_sdp_service_settings(self, recovery_data: RecoveryData) -> SdpSettings {
        SdpSettings {
            declaration_id: self.user.declaration_id,
            wallet_config: SdpWalletConfig {
                funding_key_id: self.user.wallet.funding_key_id,
                max_tx_fee: self.user.wallet.max_tx_fee,
            },
            active_message_tracker: ActiveMessageTrackerConfig {
                status_check_interval_in_tip_changes: self
                    .user
                    .active_message_tracker
                    .status_check_interval_in_tip_changes,
            },
            recovery_data,
        }
    }
}
