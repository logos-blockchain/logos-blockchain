use std::collections::HashMap;

use lb_key_management_system_service::{
    backend::preload::KeyId,
    hd::{Mnemonic, Path},
    keys::{Ed25519Key, Key, ZkKey},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub backend: PreloadKmsBackendSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PreloadKmsBackendSettings {
    /// The BIP-39 mnemonic that the [`KeyEntry::Hd`] keys are derived from.
    pub mnemonic: Option<Mnemonic>,
    pub keys: HashMap<KeyId, KeyEntry>,
}

/// A key to load into the KMS, either given as is or derived from the
/// mnemonic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyEntry {
    Ed25519(Ed25519Key),
    Zk(ZkKey),
    /// The ZK key of the leaf at the HD path.
    Hd(Path),
}

impl From<Key> for KeyEntry {
    fn from(key: Key) -> Self {
        match &key {
            Key::Ed25519(key) => Self::Ed25519(key.clone()),
            Key::Zk(key) => Self::Zk(key.clone()),
        }
    }
}

impl From<Ed25519Key> for KeyEntry {
    fn from(key: Ed25519Key) -> Self {
        Self::Ed25519(key)
    }
}

impl From<ZkKey> for KeyEntry {
    fn from(key: ZkKey) -> Self {
        Self::Zk(key)
    }
}

#[cfg(test)]
mod tests {
    use num_bigint::BigUint;
    use rand::rngs::OsRng;

    use super::*;

    #[test]
    fn serde_keys_from_yaml() {
        let settings = PreloadKmsBackendSettings {
            mnemonic: Some(Mnemonic::generate()),
            keys: [
                (
                    "ed25519".into(),
                    KeyEntry::Ed25519(Ed25519Key::generate(&mut OsRng)),
                ),
                (
                    "zk".into(),
                    KeyEntry::Zk(ZkKey::new(BigUint::from_bytes_le(&[1u8; 32]).into())),
                ),
                (
                    "hd".into(),
                    KeyEntry::Hd("m/154'/0'/0'/0'".parse().unwrap()),
                ),
            ]
            .into(),
        };

        let yaml = serde_yaml::to_string(&settings).unwrap();
        assert!(yaml.contains("hd: !Hd m/154'/0'/0'/0'"), "{yaml}");

        let deserialized: PreloadKmsBackendSettings = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(deserialized.mnemonic, settings.mnemonic);
        assert_eq!(deserialized.keys, settings.keys);
    }

    #[test]
    fn invalid_mnemonic_is_rejected() {
        let yaml = "mnemonic: abandon abandon about";
        assert!(serde_yaml::from_str::<PreloadKmsBackendSettings>(yaml).is_err());
    }
}
