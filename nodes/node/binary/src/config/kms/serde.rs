use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use lb_key_management_system_service::{backend::preload::KeyId, keys::Key};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The keystore, relative to the user config file. When it exists, the
    /// node identity, the key ids, `wallet` and `kms` are populated from it and
    /// the user config only overrides them; without it the user config has to
    /// be complete.
    pub keystore: PathBuf,
    pub backend: PreloadKmsBackendSettings,
}

impl Config {
    #[must_use]
    pub fn keystore_path(&self, config_path: &Path) -> PathBuf {
        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(&self.keystore)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            keystore: "keystore.yaml".into(),
            backend: PreloadKmsBackendSettings::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PreloadKmsBackendSettings {
    pub keys: HashMap<KeyId, Key>,
}

#[cfg(test)]
mod tests {
    use lb_key_management_system_service::keys::{Ed25519Key, Key, ZkKey};
    use num_bigint::BigUint;
    use rand::rngs::OsRng;

    use crate::config::kms::serde::PreloadKmsBackendSettings;

    #[test]
    fn serde_keys_from_yaml() {
        let preloaded_keys = PreloadKmsBackendSettings {
            keys: [
                (
                    "test1".into(),
                    Key::Ed25519(Ed25519Key::generate(&mut OsRng)),
                ),
                (
                    "test2".into(),
                    Key::Zk(ZkKey::new(BigUint::from_bytes_le(&[1u8; 32]).into())),
                ),
            ]
            .into(),
        };

        let mut serialized_output = Vec::new();
        serde_yaml::to_writer(&mut serialized_output, &preloaded_keys).unwrap();

        let deserialized_keys: PreloadKmsBackendSettings =
            serde_yaml::from_slice(&serialized_output).unwrap();

        assert_eq!(preloaded_keys.keys.len(), deserialized_keys.keys.len());
        let original_key = preloaded_keys.keys.keys().next().unwrap();
        assert!(deserialized_keys.keys.contains_key(original_key));
    }
}
