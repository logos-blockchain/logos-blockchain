use std::{collections::HashMap, fmt};

use lb_key_management_system_service::{
    backend::preload::KeyId,
    hd::{Mnemonic, Path},
    keys::{Ed25519Key, Key, ZkKey},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub backend: PreloadKmsBackendSettings,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PreloadKmsBackendSettings {
    /// The BIP-39 mnemonic that the HD keys are derived from
    pub mnemonic: Mnemonic,
    /// The BIP-39 passphrase of the mnemonic, which is empty if not set
    #[serde(default)]
    pub passphrase: Option<String>,
    /// The keys to load under a name
    #[serde(default)]
    pub keys: HashMap<KeyId, KeyEntry>,
}

impl fmt::Debug for PreloadKmsBackendSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreloadKmsBackendSettings")
            .field("mnemonic", &self.mnemonic)
            .field(
                "passphrase",
                &self.passphrase.as_ref().map(|_| "<redacted>"),
            )
            .field("keys", &self.keys)
            .finish()
    }
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
            mnemonic: Mnemonic::generate(&mut OsRng),
            passphrase: Some("passphrase".to_owned()),
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
        let deserialized: PreloadKmsBackendSettings = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(deserialized.mnemonic, settings.mnemonic);
        assert_eq!(deserialized.passphrase, settings.passphrase);
        assert_eq!(deserialized.keys, settings.keys);
    }

    #[test]
    fn invalid_mnemonic_is_rejected() {
        let yaml = "mnemonic: abandon abandon about";
        assert!(serde_yaml::from_str::<PreloadKmsBackendSettings>(yaml).is_err());
    }

    #[test]
    fn mnemonic_is_required() {
        assert!(serde_yaml::from_str::<PreloadKmsBackendSettings>("keys: {}").is_err());
    }

    #[test]
    fn passphrase_is_not_shown() {
        let settings = PreloadKmsBackendSettings {
            mnemonic: Mnemonic::generate(&mut OsRng),
            passphrase: Some("passphrase".to_owned()),
            keys: HashMap::new(),
        };
        assert!(!format!("{settings:?}").contains("passphrase\""));
    }
}
