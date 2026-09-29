use std::collections::HashMap;

use lb_key_management_system_service::{
    backend::preload::KeyId,
    hd::{Mnemonic, Passphrase},
    keys::Key,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub backend: KmsBackendSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KmsBackendSettings {
    /// The BIP-39 mnemonic that the HD keys are derived from
    pub mnemonic: Mnemonic,
    /// The BIP-39 passphrase of the mnemonic, which is empty if not set
    #[serde(default)]
    pub passphrase: Option<Passphrase>,
    /// The keys to load under a name
    #[serde(default)]
    pub keys: HashMap<KeyId, Key>,
}

#[cfg(test)]
mod tests {
    use lb_key_management_system_service::keys::{Ed25519Key, ZkKey};
    use num_bigint::BigUint;
    use rand::rngs::OsRng;

    use super::*;

    #[test]
    fn serde_keys_from_yaml() {
        let settings = KmsBackendSettings {
            mnemonic: Mnemonic::generate(&mut OsRng),
            passphrase: Some("passphrase".into()),
            keys: [
                (
                    "ed25519".into(),
                    Key::Ed25519(Ed25519Key::generate(&mut OsRng)),
                ),
                (
                    "zk".into(),
                    Key::Zk(ZkKey::new(BigUint::from_bytes_le(&[1u8; 32]).into())),
                ),
            ]
            .into(),
        };

        let yaml = serde_yaml::to_string(&settings).unwrap();
        let deserialized: KmsBackendSettings = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(deserialized.mnemonic, settings.mnemonic);
        assert_eq!(deserialized.passphrase, settings.passphrase);
        assert_eq!(deserialized.keys, settings.keys);
    }

    #[test]
    fn invalid_mnemonic_is_rejected() {
        let yaml = "mnemonic: abandon abandon about";
        assert!(serde_yaml::from_str::<KmsBackendSettings>(yaml).is_err());
    }

    #[test]
    fn mnemonic_is_required() {
        assert!(serde_yaml::from_str::<KmsBackendSettings>("keys: {}").is_err());
    }

    #[test]
    fn passphrase_is_not_shown() {
        let settings = KmsBackendSettings {
            mnemonic: Mnemonic::generate(&mut OsRng),
            passphrase: Some("passphrase".into()),
            keys: HashMap::new(),
        };
        assert!(!format!("{settings:?}").contains("\"passphrase\""));
    }
}
