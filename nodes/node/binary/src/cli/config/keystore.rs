use std::collections::HashMap;

use lb_key_management_system_service::{
    backend::preload::KeyId,
    hd::{MasterKey, MasterSeed, Mnemonic},
    keys::{Ed25519Key, Key, UnsecuredEd25519Key, UnsecuredZkKey, ZkKey},
};
use num_bigint::BigUint;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::kms::serde::PreloadKmsBackendSettings;

const WARNING: &str = "Do not share your mnemonic and secret keys";

#[derive(Serialize, Deserialize, Hash, Eq, PartialEq, Clone, Debug)]
#[serde(transparent)]
pub struct KeyTitle(pub String);

impl KeyTitle {
    pub const BLEND_SIGNING: &str = "BlendSigning";
    pub const BLEND_ZK: &str = "BlendZk";
    pub const NETWORK_SWARM: &str = "NetworkSwarm";

    pub const PREDEFINED_ED25519: [&'static str; 2] = [Self::BLEND_SIGNING, Self::NETWORK_SWARM];
    pub const PREDEFINED_ZK: [&'static str; 1] = [Self::BLEND_ZK];
}

impl<S: Into<String>> From<S> for KeyTitle {
    fn from(s: S) -> Self {
        Self(s.into())
    }
}

#[derive(Error, Debug)]
pub enum KeystoreError {
    #[error("Key for title '{0:?}' not found in keystore")]
    NotFound(KeyTitle),

    #[error("Ed25519 key expected for '{0:?}'")]
    Ed25519Expected(KeyTitle),

    #[error("Zk key expected for '{0:?}'")]
    ZkExpected(KeyTitle),
}

/// The secrets of a node: the mnemonic that its wallet keys are derived
/// from, and the keys that are not derived from it.
///
/// The id of a key in the KMS is its title.
#[derive(Serialize, Deserialize)]
pub struct Keystore {
    /// The BIP-39 mnemonic that the wallet keys are derived from
    mnemonic: Mnemonic,
    /// The BIP-39 passphrase of the mnemonic, which is empty if not set
    #[serde(default)]
    passphrase: Option<String>,
    secret_keys: HashMap<KeyTitle, Key>,

    #[serde(rename = "WARNING")]
    warning: String,
}

impl Keystore {
    /// Creates a keystore with the mnemonic and newly generated predefined
    /// keys.
    #[must_use]
    pub fn new(mnemonic: Mnemonic, passphrase: Option<String>) -> Self {
        let mut keystore = Self {
            mnemonic,
            passphrase,
            secret_keys: HashMap::new(),
            warning: WARNING.to_owned(),
        };

        for title in KeyTitle::PREDEFINED_ED25519 {
            keystore.generate_ed25519(title);
        }

        for title in KeyTitle::PREDEFINED_ZK {
            keystore.generate_zk(title);
        }

        keystore
    }

    pub fn set(&mut self, name: impl Into<KeyTitle>, key: impl Into<Key>) {
        self.secret_keys.insert(name.into(), key.into());
    }

    #[must_use]
    pub fn get(&self, name: impl Into<KeyTitle>) -> Option<(KeyId, Key)> {
        let title = name.into();
        let key = self.secret_keys.get(&title)?;
        Some((title.0, key.clone()))
    }

    /// The ids of all keys, in a stable order.
    #[must_use]
    pub fn key_ids(&self) -> Vec<KeyId> {
        let mut key_ids = self
            .secret_keys
            .keys()
            .map(|title| title.0.clone())
            .collect::<Vec<_>>();
        key_ids.sort();
        key_ids
    }

    /// The KMS settings that load every key of the keystore.
    #[must_use]
    pub fn kms_backend_settings(&self) -> PreloadKmsBackendSettings {
        PreloadKmsBackendSettings {
            mnemonic: self.mnemonic.clone(),
            passphrase: self.passphrase.clone(),
            keys: self
                .secret_keys
                .iter()
                .map(|(title, key)| (title.0.clone(), key.clone()))
                .collect(),
        }
    }

    /// The master key that the wallet keys are derived from
    #[must_use]
    pub fn master_key(&self) -> MasterKey {
        let passphrase = self.passphrase.as_deref().unwrap_or_default();
        MasterSeed::from_mnemonic(&self.mnemonic, passphrase).to_key()
    }

    pub fn get_ed25519(
        &self,
        title: impl Into<KeyTitle>,
    ) -> Result<(KeyId, UnsecuredEd25519Key), KeystoreError> {
        let title = title.into();
        let (key_id, key) = self
            .get(title.clone())
            .ok_or_else(|| KeystoreError::NotFound(title.clone()))?;

        match &key {
            Key::Ed25519(inner_key) => Ok((key_id, inner_key.clone().into_unsecured())),
            Key::Zk(_) => Err(KeystoreError::Ed25519Expected(title)),
        }
    }

    pub fn get_zk(
        &self,
        title: impl Into<KeyTitle>,
    ) -> Result<(KeyId, UnsecuredZkKey), KeystoreError> {
        let title = title.into();
        let (id, key) = self
            .get(title.clone())
            .ok_or_else(|| KeystoreError::NotFound(title.clone()))?;

        match &key {
            Key::Zk(inner_key) => Ok((id, inner_key.clone().into_unsecured())),
            Key::Ed25519(_) => Err(KeystoreError::ZkExpected(title)),
        }
    }

    pub fn generate_ed25519(&mut self, title: impl Into<KeyTitle>) -> (KeyId, UnsecuredEd25519Key) {
        let title = title.into();
        let secure_key = Ed25519Key::generate(&mut OsRng);
        let unsecured = secure_key.clone().into_unsecured();

        self.set(title.clone(), Key::Ed25519(secure_key));
        (title.0, unsecured)
    }

    pub fn generate_zk(&mut self, title: impl Into<KeyTitle>) -> (KeyId, UnsecuredZkKey) {
        let title = title.into();
        let secure_key = generate_zk_key_from_random_bytes();
        let unsecured = secure_key.clone().into_unsecured();

        self.set(title.clone(), Key::Zk(secure_key));
        (title.0, unsecured)
    }

    pub fn remove(&mut self, title: impl Into<KeyTitle>) -> Option<(KeyId, Key)> {
        let title = title.into();
        let key = self.secret_keys.remove(&title)?;
        Some((title.0, key))
    }
}

impl Default for Keystore {
    /// Creates a keystore from a newly generated mnemonic.
    fn default() -> Self {
        Self::new(Mnemonic::generate(&mut OsRng), None)
    }
}

fn generate_zk_key_from_random_bytes() -> ZkKey {
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut OsRng, &mut bytes);
    ZkKey::from(BigUint::from_bytes_le(&bytes))
}

#[cfg(test)]
mod tests {
    use lb_groth16::fr_to_bytes;

    use super::*;

    // Test vectors of the spec
    const MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    // The ZK key at m/154'/0'/0'/0'
    const RECEIVE_0_ZK_KEY: &str =
        "5e09bf4ce6b3f42970104a6f5940104407f98da0eb946104c13fb4f94c011f16";

    #[test]
    fn wallet_keys_are_derived_from_mnemonic() {
        let keystore = Keystore::new(MNEMONIC.parse().unwrap(), None);
        assert_eq!(receive_0_key(&keystore), RECEIVE_0_ZK_KEY);
    }

    #[test]
    fn passphrase_changes_wallet_keys() {
        let keystore = Keystore::new(MNEMONIC.parse().unwrap(), Some("passphrase".to_owned()));
        assert_ne!(receive_0_key(&keystore), RECEIVE_0_ZK_KEY);
    }

    #[test]
    fn keys_are_identified_by_their_titles() {
        let keystore = Keystore::default();
        assert_eq!(
            keystore.key_ids(),
            [
                KeyTitle::BLEND_SIGNING,
                KeyTitle::BLEND_ZK,
                KeyTitle::NETWORK_SWARM
            ]
        );
        let (key_id, _) = keystore.get(KeyTitle::BLEND_ZK).unwrap();
        assert_eq!(key_id, KeyTitle::BLEND_ZK);
    }

    #[test]
    fn default_generates_a_new_mnemonic() {
        assert_ne!(Keystore::default().mnemonic, Keystore::default().mnemonic);
    }

    #[test]
    fn serde_from_yaml() {
        let keystore = Keystore::new(MNEMONIC.parse().unwrap(), None);
        let yaml = serde_yaml::to_string(&keystore).unwrap();
        let deserialized: Keystore = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(deserialized.mnemonic, keystore.mnemonic);
        assert_eq!(deserialized.secret_keys, keystore.secret_keys);
    }

    #[test]
    fn kms_backend_settings_load_every_key() {
        let keystore = Keystore::new(MNEMONIC.parse().unwrap(), Some("passphrase".to_owned()));
        let settings = keystore.kms_backend_settings();
        assert_eq!(settings.keys.len(), keystore.secret_keys.len());
        for title in keystore.secret_keys.keys() {
            let (key_id, key) = keystore.get(title.clone()).unwrap();
            assert_eq!(settings.keys.get(&key_id), Some(&key));
        }
    }

    fn receive_0_key(keystore: &Keystore) -> String {
        let key = keystore
            .master_key()
            .derive_key(&"m/154'/0'/0'/0'".parse().unwrap())
            .to_zk_key();
        hex::encode(fr_to_bytes(key.as_fr()))
    }
}
