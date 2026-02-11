use lb_key_management_system_service::backend::preload::PreloadKMSBackendSettings;

use crate::config::kms::serde::Config;

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl From<ServiceConfig> for PreloadKMSBackendSettings {
    fn from(value: ServiceConfig) -> Self {
        Self {
            keys: value.user.backend.keys,
        }
    }
}
