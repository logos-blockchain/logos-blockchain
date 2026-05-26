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

#[derive(Serialize, Deserialize, Hash, Eq, PartialEq, Clone, Copy, Debug)]
pub enum KeyTitle {
    NetworkSwarm,
    BlendSigning,
    BlendZk,
    LeaderFunding,
    SdpFunding,
    BlendFunding,
    VaucherMaster,
}

impl KeyTitle {
    pub const ALL: [Self; 7] = [
        Self::NetworkSwarm,
        Self::BlendSigning,
        Self::BlendZk,
        Self::LeaderFunding,
        Self::SdpFunding,
        Self::BlendFunding,
        Self::VaucherMaster,
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
    keys: HashMap<KeyTitle, Key>,
}

impl Keystore {
    pub fn set(&mut self, title: KeyTitle, key: Key) {
        self.keys.insert(title, key);
    }

    #[must_use]
    pub fn get(&self, title: KeyTitle) -> Option<(KeyId, &Key)> {
        let key = self.keys.get(&title)?;
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
            | KeyTitle::BlendFunding
            | KeyTitle::VaucherMaster => Key::Zk(generate_zk_key_from_random_bytes()),
        }
    }
}

impl Default for Keystore {
    fn default() -> Self {
        let mut keystore = Self {
            keys: HashMap::new(),
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
