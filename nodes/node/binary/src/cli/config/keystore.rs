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

use crate::config::kms::serde::{KeyEntry, PreloadKmsBackendSettings};

const WARNING: &str = "Do not share your mnemonic and secret keys";

#[derive(Serialize, Deserialize, Hash, Eq, PartialEq, Clone, Debug)]
#[serde(transparent)]
pub struct KeyTitle(pub String);

impl KeyTitle {
    pub const BLEND_SIGNING: &str = "BlendSigning";
    pub const BLEND_ZK: &str = "BlendZk";
    pub const LEADER_FUNDING: &str = "LeaderFunding";
    pub const NETWORK_SWARM: &str = "NetworkSwarm";
    pub const POW_CLAIM: &str = "PoWClaim";
    pub const SDP_FUNDING: &str = "SdpFunding";
    pub const VOUCHER_MASTER: &str = "VoucherMaster";
    pub const STAKE: &str = "Stake";

    pub const PREDEFINED_ED25519: [&'static str; 2] = [Self::BLEND_SIGNING, Self::NETWORK_SWARM];
    pub const PREDEFINED_ZK: [&'static str; 1] = [Self::BLEND_ZK];
    /// The ZK keys derived from the mnemonic, with their HD paths.
    pub const PREDEFINED_HD: [(&'static str, &'static str); 5] = [
        (Self::LEADER_FUNDING, "m/154'/0'/0'/0'"),
        (Self::SDP_FUNDING, "m/154'/0'/0'/1'"),
        (Self::STAKE, "m/154'/0'/0'/2'"),
        (Self::POW_CLAIM, "m/154'/0'/0'/3'"),
        (Self::VOUCHER_MASTER, "m/154'/0'/2'"),
    ];

    /// The id under which the key is loaded into the KMS.
    fn key_id(&self) -> KeyId {
        self.0.clone()
    }
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

#[derive(Serialize, Deserialize)]
pub struct Keystore {
    /// The BIP-39 mnemonic that the [`KeyEntry::Hd`] keys are derived from.
    mnemonic: Mnemonic,
    /// The BIP-39 passphrase of the mnemonic, which is empty if not set.
    #[serde(default)]
    passphrase: Option<String>,
    secret_keys: HashMap<KeyTitle, KeyEntry>,

    #[serde(rename = "WARNING")]
    warning: String,
}

impl Keystore {
    /// Creates a keystore with the predefined keys, deriving the HD ones from
    /// `mnemonic` and `passphrase`.
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

        for (title, path) in KeyTitle::PREDEFINED_HD {
            let path = path.parse().expect("Predefined HD path is valid");
            keystore.set(title, KeyEntry::Hd(path));
        }

        keystore
    }

    pub fn set(&mut self, name: impl Into<KeyTitle>, key: impl Into<KeyEntry>) {
        self.secret_keys.insert(name.into(), key.into());
    }

    #[must_use]
    pub fn get(&self, name: impl Into<KeyTitle>) -> Option<(KeyId, Key)> {
        let title = name.into();
        let entry = self.secret_keys.get(&title)?;
        let key = resolve(entry, &self.master_key());
        Some((title.key_id(), key))
    }

    /// The KMS settings that load every key of the keystore.
    #[must_use]
    pub fn kms_backend_settings(&self) -> PreloadKmsBackendSettings {
        PreloadKmsBackendSettings {
            mnemonic: Some(self.mnemonic.clone()),
            passphrase: self.passphrase.clone(),
            keys: self
                .secret_keys
                .iter()
                .map(|(title, entry)| (title.key_id(), entry.clone()))
                .collect(),
        }
    }

    fn master_key(&self) -> MasterKey {
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

    pub fn get_all_zk(&self) -> impl Iterator<Item = (KeyId, UnsecuredZkKey)> + '_ {
        let master = self.master_key();
        self.secret_keys.iter().filter_map(move |(title, entry)| {
            let key = resolve(entry, &master);
            match &key {
                Key::Zk(inner_key) => {
                    let id = title.key_id();
                    let unsecured = inner_key.clone().into_unsecured();
                    Some((id, unsecured))
                }
                Key::Ed25519(_) => None,
            }
        })
    }

    pub fn generate_ed25519(&mut self, title: impl Into<KeyTitle>) -> (KeyId, UnsecuredEd25519Key) {
        let title = title.into();
        let secure_key = Ed25519Key::generate(&mut OsRng);
        let unsecured = secure_key.clone().into_unsecured();

        self.set(title.clone(), Key::Ed25519(secure_key));
        (title.key_id(), unsecured)
    }

    pub fn generate_zk(&mut self, title: impl Into<KeyTitle>) -> (KeyId, UnsecuredZkKey) {
        let title = title.into();
        let secure_key = generate_zk_key_from_random_bytes();
        let unsecured = secure_key.clone().into_unsecured();

        self.set(title.clone(), Key::Zk(secure_key));
        (title.key_id(), unsecured)
    }

    pub fn remove(&mut self, title: impl Into<KeyTitle>) -> Option<(KeyId, Key)> {
        let title = title.into();
        let entry = self.secret_keys.remove(&title)?;
        let key = resolve(&entry, &self.master_key());
        Some((title.key_id(), key))
    }
}

impl Default for Keystore {
    /// Creates a keystore from a newly generated mnemonic.
    fn default() -> Self {
        Self::new(Mnemonic::generate(), None)
    }
}

fn resolve(entry: &KeyEntry, master: &MasterKey) -> Key {
    entry
        .resolve(Some(master))
        .expect("Every entry resolves with a master key")
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
    fn predefined_hd_keys_are_derived_from_mnemonic() {
        let keystore = Keystore::new(MNEMONIC.parse().unwrap(), None);
        let (_, key) = keystore.get(KeyTitle::LEADER_FUNDING).unwrap();
        let Key::Zk(key) = &key else {
            panic!("expected a ZK key");
        };
        assert_eq!(hex::encode(fr_to_bytes(key.as_fr())), RECEIVE_0_ZK_KEY);
    }

    #[test]
    fn get_all_zk_includes_hd_keys() {
        let keystore = Keystore::new(MNEMONIC.parse().unwrap(), None);
        assert_eq!(
            keystore.get_all_zk().count(),
            KeyTitle::PREDEFINED_ZK.len() + KeyTitle::PREDEFINED_HD.len()
        );
    }

    #[test]
    fn passphrase_changes_hd_keys() {
        let keystore = Keystore::new(MNEMONIC.parse().unwrap(), Some("passphrase".to_owned()));
        let (_, key) = keystore.get(KeyTitle::LEADER_FUNDING).unwrap();
        let Key::Zk(key) = &key else {
            panic!("expected a ZK key");
        };
        assert_ne!(hex::encode(fr_to_bytes(key.as_fr())), RECEIVE_0_ZK_KEY);
    }

    #[test]
    fn key_id_is_the_title() {
        let keystore = Keystore::new(MNEMONIC.parse().unwrap(), None);

        let (key_id, _) = keystore.get(KeyTitle::LEADER_FUNDING).unwrap();
        assert_eq!(key_id, KeyTitle::LEADER_FUNDING);

        let kms_keys = keystore.kms_backend_settings().keys;
        assert!(kms_keys.contains_key(KeyTitle::LEADER_FUNDING));
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
        let keys = keystore.kms_backend_settings().resolve_keys().unwrap();
        assert_eq!(keys.len(), keystore.secret_keys.len());
        for title in keystore.secret_keys.keys() {
            let (key_id, key) = keystore.get(title.clone()).unwrap();
            assert_eq!(keys.get(&key_id), Some(&key));
        }
    }
}
