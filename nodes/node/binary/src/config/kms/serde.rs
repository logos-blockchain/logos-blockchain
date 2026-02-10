use std::collections::HashMap;

use lb_key_management_system_service::{backend::preload::KeyId, keys::Key};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub backend: PreloadKmsBackendSettings,
}

#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(feature = "testing", derive(Serialize))]
pub struct PreloadKmsBackendSettings {
    pub keys: HashMap<KeyId, Key>,
}

impl From<PreloadKmsBackendSettings>
    for lb_key_management_system_service::backend::preload::PreloadKMSBackendSettings
{
    fn from(value: PreloadKmsBackendSettings) -> Self {
        Self { keys: value.keys }
    }
}

impl From<lb_key_management_system_service::backend::preload::PreloadKMSBackendSettings>
    for PreloadKmsBackendSettings
{
    fn from(
        value: lb_key_management_system_service::backend::preload::PreloadKMSBackendSettings,
    ) -> Self {
        Self { keys: value.keys }
    }
}
