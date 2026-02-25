use lb_sdp_service::{SdpSettings, wallet::SdpWalletConfig};

use crate::config::sdp::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl From<ServiceConfig> for SdpSettings {
    fn from(value: ServiceConfig) -> Self {
        Self {
            declaration_id: value.user.declaration_id,
            wallet_config: SdpWalletConfig {
                funding_pk: value.user.wallet.funding_pk,
                max_tx_fee: value.user.wallet.max_tx_fee,
            },
        }
    }
}
