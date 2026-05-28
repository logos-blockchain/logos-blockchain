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

#[derive(Serialize, Deserialize, Hash, Eq, PartialEq, Clone, Copy, Debug)]
pub enum KeyTitle {
    BlendSigning,
    BlendZk,
    LeaderFunding,
    NetworkSwarm,
    SdpFunding,
    VaucherMaster,
    Stake,
}

impl KeyTitle {
    pub const ALL: [Self; 7] = [
        Self::BlendSigning,
        Self::BlendZk,
        Self::LeaderFunding,
        Self::NetworkSwarm,
        Self::SdpFunding,
        Self::VaucherMaster,
        Self::Stake,
    ];
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
    pub fn set(&mut self, title: KeyTitle, key: Key) {
        self.public_keys.insert(title, key_id(&key));
        self.secret_keys.insert(title, key);
    }

    #[must_use]
    pub fn get(&self, title: KeyTitle) -> Option<(KeyId, &Key)> {
        let key = self.secret_keys.get(&title)?;
        Some((key_id(key), key))
    }

    pub fn get_ed25519(
        &self,
        title: KeyTitle,
    ) -> Result<(KeyId, UnsecuredEd25519Key), KeystoreError> {
        let (id, generic_key) = self.get(title).ok_or(KeystoreError::NotFound(title))?;

        match generic_key {
            Key::Ed25519(inner_key) => Ok((id, inner_key.clone().into_unsecured())),
            Key::Zk(_) => Err(KeystoreError::Ed25519Expected(title)),
        }
    }

    pub fn get_zk(&self, title: KeyTitle) -> Result<(KeyId, UnsecuredZkKey), KeystoreError> {
        let (id, generic_key) = self.get(title).ok_or(KeystoreError::NotFound(title))?;

        match generic_key {
            Key::Zk(inner_key) => Ok((id, inner_key.clone().into_unsecured())),
            Key::Ed25519(_) => Err(KeystoreError::ZkExpected(title)),
        }
    }

    #[must_use]
    pub fn generate(title: KeyTitle) -> Key {
        match title {
            KeyTitle::NetworkSwarm | KeyTitle::BlendSigning => {
                Key::Ed25519(Ed25519Key::generate(&mut OsRng))
            }
            KeyTitle::BlendZk
            | KeyTitle::LeaderFunding
            | KeyTitle::SdpFunding
            | KeyTitle::VaucherMaster
            | KeyTitle::Stake => Key::Zk(generate_zk_key_from_random_bytes()),
        }
    }
}

impl Default for Keystore {
    fn default() -> Self {
        let mut keystore = Self {
            public_keys: HashMap::new(),
            secret_keys: HashMap::new(),
            warning: WARNING.to_owned(),
        };

        for title in KeyTitle::ALL {
            keystore.set(title, Self::generate(title));
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
