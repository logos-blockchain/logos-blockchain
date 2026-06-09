use std::collections::HashMap;

use lb_groth16::fr_to_bytes;
use lb_key_management_system_service::{
    backend::preload::KeyId,
    keys::{
        Ed25519Key, Key, UnsecuredEd25519Key, UnsecuredZkKey, ZkKey, secured_key::SecuredKey as _,
    },
};
use num_bigint::BigUint;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const WARNING: &str = "Do not share your secret keys";

#[derive(Serialize, Deserialize, Hash, Eq, PartialEq, Clone, Debug)]
#[serde(transparent)]
pub struct KeyTitle(pub String);

impl KeyTitle {
    pub const BLEND_SIGNING: &str = "BlendSigning";
    pub const BLEND_ZK: &str = "BlendZk";
    pub const LEADER_FUNDING: &str = "LeaderFunding";
    pub const NETWORK_SWARM: &str = "NetworkSwarm";
    pub const SDP_FUNDING: &str = "SdpFunding";
    pub const VAUCHER_MASTER: &str = "VaucherMaster";
    pub const STAKE: &str = "Stake";

    pub const PREDEFINED_ED25519: [&'static str; 2] = [Self::BLEND_SIGNING, Self::NETWORK_SWARM];
    pub const PREDEFINED_ZK: [&'static str; 5] = [
        Self::BLEND_ZK,
        Self::LEADER_FUNDING,
        Self::SDP_FUNDING,
        Self::VAUCHER_MASTER,
        Self::STAKE,
    ];
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
    // Convenience mapping for users to inspect when serialized.
    public_keys: HashMap<KeyTitle, KeyId>,
    secret_keys: HashMap<KeyTitle, Key>,

    #[serde(rename = "WARNING")]
    warning: String,
}

impl Keystore {
    pub fn set(&mut self, name: impl Into<KeyTitle>, key: Key) {
        let key_name = name.into();
        self.public_keys.insert(key_name.clone(), key_id(&key));
        self.secret_keys.insert(key_name, key);
    }

    #[must_use]
    pub fn get(&self, name: impl Into<KeyTitle>) -> Option<(KeyId, &Key)> {
        self.secret_keys
            .get_key_value(&name.into())
            .map(|(_, v)| (key_id(v), v))
    }

    pub fn get_all(&self) -> impl Iterator<Item = (KeyId, &Key)> {
        self.secret_keys.values().map(|key| (key_id(key), key))
    }

    pub fn get_ed25519(
        &self,
        title: impl Into<KeyTitle>,
    ) -> Result<(KeyId, UnsecuredEd25519Key), KeystoreError> {
        let title = title.into();
        let (key_id, generic_key) = self
            .get(title.clone())
            .ok_or_else(|| KeystoreError::NotFound(title.clone()))?;

        match generic_key {
            Key::Ed25519(inner_key) => Ok((key_id, inner_key.clone().into_unsecured())),
            Key::Zk(_) => Err(KeystoreError::Ed25519Expected(title)),
        }
    }

    pub fn get_zk(
        &self,
        title: impl Into<KeyTitle>,
    ) -> Result<(KeyId, UnsecuredZkKey), KeystoreError> {
        let title = title.into();
        let (id, generic_key) = self
            .get(title.clone())
            .ok_or_else(|| KeystoreError::NotFound(title.clone()))?;

        match generic_key {
            Key::Zk(inner_key) => Ok((id, inner_key.clone().into_unsecured())),
            Key::Ed25519(_) => Err(KeystoreError::ZkExpected(title)),
        }
    }

    pub fn get_all_zk(&self) -> impl Iterator<Item = (KeyId, UnsecuredZkKey)> + '_ {
        self.secret_keys
            .values()
            .filter_map(|generic_key| match generic_key {
                Key::Zk(inner_key) => {
                    let id = key_id(generic_key);
                    let unsecured = inner_key.clone().into_unsecured();
                    Some((id, unsecured))
                }
                Key::Ed25519(_) => None,
            })
    }

    pub fn get_all_ed25519(&self) -> impl Iterator<Item = (KeyId, UnsecuredEd25519Key)> + '_ {
        self.secret_keys
            .values()
            .filter_map(|generic_key| match generic_key {
                Key::Ed25519(inner_key) => {
                    let id = key_id(generic_key);
                    let unsecured = inner_key.clone().into_unsecured();
                    Some((id, unsecured))
                }
                Key::Zk(_) => None,
            })
    }

    pub fn generate_ed25519(&mut self, title: impl Into<KeyTitle>) -> (KeyId, UnsecuredEd25519Key) {
        let title = title.into();
        let secure_key = Ed25519Key::generate(&mut OsRng);
        let unsecured = secure_key.clone().into_unsecured();

        self.set(title.clone(), Key::Ed25519(secure_key));
        (key_id(&self.secret_keys[&title]), unsecured)
    }

    pub fn generate_zk(&mut self, title: impl Into<KeyTitle>) -> (KeyId, UnsecuredZkKey) {
        let title = title.into();
        let secure_key = generate_zk_key_from_random_bytes();
        let unsecured = secure_key.clone().into_unsecured();

        self.set(title.clone(), Key::Zk(secure_key));
        (key_id(&self.secret_keys[&title]), unsecured)
    }

    pub fn remove(&mut self, title: impl Into<KeyTitle>) -> Option<(KeyId, Key)> {
        let title = title.into();
        self.public_keys.remove(&title);
        self.secret_keys.remove(&title).map(|v| (key_id(&v), v))
    }
}

impl Default for Keystore {
    fn default() -> Self {
        let mut keystore = Self {
            public_keys: HashMap::new(),
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
}

fn key_id(key: &Key) -> KeyId {
    let key_id_bytes = match key {
        Key::Ed25519(ed25519_secret_key) => ed25519_secret_key.as_public_key().to_bytes(),
        Key::Zk(zk_secret_key) => fr_to_bytes(zk_secret_key.as_public_key().as_fr()),
    };
    hex::encode(key_id_bytes)
}

fn generate_zk_key_from_random_bytes() -> ZkKey {
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut OsRng, &mut bytes);
    ZkKey::from(BigUint::from_bytes_le(&bytes))
}
