use lb_wallet_service::WalletServiceSettings;

use crate::config::wallet::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl From<ServiceConfig> for WalletServiceSettings {
    fn from(value: ServiceConfig) -> Self {
        Self {
            known_keys: value.user.known_keys,
            voucher_master_key_id: value.user.voucher_master_key_id,
            recovery_path: value.user.recovery_path,
        }
    }
}
