use lb_key_management_system_service::backend::preload::PreloadKMSBackendSettings;

use crate::config::kms::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl From<ServiceConfig> for PreloadKMSBackendSettings {
    fn from(value: ServiceConfig) -> Self {
        Self {
            // TODO: pass `value.user.backend.mnemonic` and `value.user.backend.passphrase`
            // to the new KMS backend settings
            keys: value.user.backend.static_keys,
        }
    }
}
