use std::collections::HashMap;

use lb_key_management_system_keys::hd::{
    self, ExtendedSecretKey, MasterSeed, Mnemonic, Passphrase,
};
use lb_key_management_system_service::{backend::preload::KeyId, keys::Key};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub backend: KmsBackendSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KmsBackendSettings {
    /// The BIP-39 mnemonic that the HD keys are derived from
    pub mnemonic: Mnemonic,
    /// The BIP-39 passphrase of the mnemonic
    #[serde(default)]
    pub passphrase: Option<Passphrase>,
    #[serde(default)]
    pub static_keys: HashMap<KeyId, Key>,
}

impl KmsBackendSettings {
    #[must_use]
    pub fn derive_key(&self, path: &hd::Path) -> ExtendedSecretKey {
        MasterSeed::from_mnemonic(&self.mnemonic, self.passphrase.as_ref())
            .to_key()
            .derive_key(path)
    }
}

#[cfg(test)]
mod tests {
    use lb_key_management_system_keys::hd::Mnemonic;
    use lb_key_management_system_service::keys::{Ed25519Key, Key, ZkKey};
    use num_bigint::BigUint;
    use rand::rngs::OsRng;

    use crate::config::kms::serde::KmsBackendSettings;

    #[test]
    fn serde_keys_from_yaml() {
        let settings = KmsBackendSettings {
            mnemonic: Mnemonic::generate(&mut OsRng),
            passphrase: Some("passphrase".into()),
            static_keys: [
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

        let mut serialized = Vec::new();
        serde_yaml::to_writer(&mut serialized, &settings).unwrap();

        let deserialized: KmsBackendSettings = serde_yaml::from_slice(&serialized).unwrap();

        assert_eq!(deserialized.mnemonic, settings.mnemonic);
        assert_eq!(deserialized.passphrase, settings.passphrase);
        assert_eq!(settings.static_keys.len(), deserialized.static_keys.len());
        let original_key = settings.static_keys.keys().next().unwrap();
        assert!(deserialized.static_keys.contains_key(original_key));
    }
}
