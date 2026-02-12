use std::collections::HashMap;

use lb_key_management_system_service::{backend::preload::KeyId, keys::Key};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, Default)]
#[cfg_attr(
    any(test, feature = "testing", feature = "config-gen"),
    derive(serde::Serialize)
)]
#[serde(default)]
pub struct Config {
    pub backend: PreloadKmsBackendSettings,
}

#[derive(Clone, Debug, Deserialize, Default)]
#[cfg_attr(
    any(test, feature = "testing", feature = "config-gen"),
    derive(serde::Serialize)
)]
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
