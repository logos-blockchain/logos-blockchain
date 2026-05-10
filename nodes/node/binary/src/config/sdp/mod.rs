use std::path::PathBuf;

use lb_sdp_service::{SdpSettings, wallet::SdpWalletConfig};

use crate::config::{StateConfig, sdp::serde::Config};

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    #[must_use]
    pub fn into_sdp_service_settings(self, state_config: &StateConfig) -> SdpSettings {
        let recovery_path = state_config.get_path_for_recovery_state(
            PathBuf::new()
                .join("mempool")
                .join("recovery")
                .with_extension("json")
                .as_path(),
        );

        SdpSettings {
            declaration_id: self.user.declaration_id,
            wallet_config: SdpWalletConfig {
                funding_pk: self.user.wallet.funding_pk,
                max_tx_fee: self.user.wallet.max_tx_fee,
            },
            recovery_path,
        }
    }
}
