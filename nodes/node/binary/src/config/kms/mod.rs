use lb_key_management_system_service::backend::hd_and_preload::HdAndPreloadKMSBackendSettings;

use crate::config::kms::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl From<ServiceConfig> for HdAndPreloadKMSBackendSettings {
    fn from(value: ServiceConfig) -> Self {
        let backend = value.user.backend;
        Self {
            mnemonic: backend.mnemonic,
            passphrase: backend.passphrase,
            static_keys: backend.static_keys,
        }
    }
}
