use std::path::PathBuf;

use lb_wallet_service::WalletServiceSettings;

use crate::config::{state::Config as StateConfig, wallet::serde::Config};

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    #[must_use]
    pub fn into_wallet_service_settings(self, state_config: &StateConfig) -> WalletServiceSettings {
        let recovery_path = state_config.get_path_for_recovery_state(
            PathBuf::new()
                .join("wallet")
                .join("recovery")
                .with_extension("json")
                .as_path(),
        );
        WalletServiceSettings {
            known_keys: self.user.known_keys,
            voucher_master_key_id: self.user.voucher_master_key_id,
            recovery_path,
        }
    }
}
