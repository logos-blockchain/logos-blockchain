use std::path::PathBuf;

use lb_wallet_service::WalletServiceSettings;

use crate::config::{
    recovery::get_path_for_base_folder_and_file_name, storage::serde::Config as StorageConfig,
    wallet::serde::Config,
};

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    #[must_use]
    pub fn into_wallet_service_settings(
        self,
        storage_config: &StorageConfig,
    ) -> WalletServiceSettings {
        let recovery_path = get_path_for_base_folder_and_file_name(
            &storage_config.backend.path,
            PathBuf::new()
                .join("wallet")
                .with_file_name("recovery")
                .with_added_extension("json")
                .as_path(),
        );
        WalletServiceSettings {
            known_keys: self.user.known_keys,
            voucher_master_key_id: self.user.voucher_master_key_id,
            recovery_path,
        }
    }
}
