use lb_sdp_service::{SdpSettings, wallet::SdpWalletConfig};
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
            recovery_data,
        }
    }
}
