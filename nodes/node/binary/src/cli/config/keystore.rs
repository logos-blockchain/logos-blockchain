use std::collections::HashMap;

use lb_key_management_system_service::keys::{Ed25519Key, Key, ZkKey};
use num_bigint::BigUint;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Hash, Eq, PartialEq, Clone, Copy, Debug)]
pub enum KeyTitle {
    BlendSigning,
    BlendZk,
    LeaderFunding,
    SdpFunding,
    BlendFunding,
    VaucherMaster,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Keystore {
    keys: HashMap<KeyTitle, Key>,
}

impl Keystore {
    pub fn set(&mut self, title: KeyTitle, key: Key) {
        self.keys.insert(title, key);
    }

    pub fn get(&self, title: KeyTitle) -> Option<&Key> {
        self.keys.get(&title)
    }

    pub fn generate(&mut self, title: KeyTitle) -> &Key {
        let key = match title {
            KeyTitle::BlendSigning => Key::Ed25519(Ed25519Key::generate(&mut OsRng)),
            KeyTitle::BlendZk
            | KeyTitle::LeaderFunding
            | KeyTitle::SdpFunding
            | KeyTitle::BlendFunding
            | KeyTitle::VaucherMaster => Key::Zk(generate_zk_key_from_random_bytes()),
        };

        self.keys.insert(title, key);
        self.keys.get(&title).unwrap()
    }
}

fn generate_zk_key_from_random_bytes() -> ZkKey {
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut OsRng, &mut bytes);
    ZkKey::from(BigUint::from_bytes_le(&bytes))
}
