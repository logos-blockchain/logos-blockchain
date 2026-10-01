use std::collections::{HashMap, HashSet};

use lb_groth16::fr_to_bytes;
use lb_key_management_system_keys::hd::{HardenedIndex, MasterSeed, Mnemonic, Passphrase};
use lb_key_management_system_service::{
    backend::preload::KeyId,
    keys::{
        Ed25519Key, Key, UnsecuredEd25519Key, UnsecuredZkKey, ZkKey, ZkPublicKey,
        secured_key::SecuredKey as _,
    },
};
use lb_wallet_service::hd;
use num_bigint::BigUint;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::kms::serde::KmsBackendSettings;

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

    // Keys that a keystore migrated from v0.3.0 holds besides the predefined
    // ones. They are kept, since they may hold funds.
    pub const LEGACY_LEADER_FUNDING: &str = "LegacyLeaderFunding";
    pub const LEGACY_POW_CLAIM: &str = "LegacyPoWClaim";
    pub const LEGACY_SDP_FUNDING: &str = "LegacySdpFunding";
    pub const LEGACY_STAKE: &str = "LegacyStake";
    pub const LEGACY_VOUCHER_MASTER: &str = "LegacyVoucherMaster";
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
    /// The BIP-39 mnemonic that the wallet keys are derived from
    mnemonic: Mnemonic,
    /// The BIP-39 passphrase of the mnemonic
    passphrase: Option<Passphrase>,

    /// Secret keys outside of HD tree.
    static_keys: HashMap<KeyTitle, Key>,

    #[serde(rename = "WARNING")]
    warning: String,
}

impl Keystore {
    pub fn set_static_key(&mut self, name: impl Into<KeyTitle>, key: Key) {
        let key_name = name.into();
        self.static_keys.insert(key_name, key);
    }

    #[must_use]
    pub fn get_static_key(&self, name: impl Into<KeyTitle>) -> Option<(KeyId, &Key)> {
        self.static_keys
            .get(&name.into())
            .map(|key| (key_id(key), key))
    }

    pub fn get_static_keys(&self) -> impl Iterator<Item = (KeyId, &Key)> {
        self.static_keys.values().map(|key| (key_id(key), key))
    }

    pub fn get_ed25519_static_key(
        &self,
        title: impl Into<KeyTitle>,
    ) -> Result<(KeyId, UnsecuredEd25519Key), KeystoreError> {
        let title = title.into();
        let (key_id, generic_key) = self
            .get_static_key(title.clone())
            .ok_or_else(|| KeystoreError::NotFound(title.clone()))?;

        match generic_key {
            Key::Ed25519(inner_key) => Ok((key_id, inner_key.clone().into_unsecured())),
            Key::Zk(_) => Err(KeystoreError::Ed25519Expected(title)),
        }
    }

    pub fn get_zk_static_key(
        &self,
        title: impl Into<KeyTitle>,
    ) -> Result<(KeyId, UnsecuredZkKey), KeystoreError> {
        let title = title.into();
        let (id, generic_key) = self
            .get_static_key(title.clone())
            .ok_or_else(|| KeystoreError::NotFound(title.clone()))?;

        match generic_key {
            Key::Zk(inner_key) => Ok((id, inner_key.clone().into_unsecured())),
            Key::Ed25519(_) => Err(KeystoreError::ZkExpected(title)),
        }
    }

    pub fn get_all_zk_static_key(&self) -> impl Iterator<Item = (KeyId, UnsecuredZkKey)> + '_ {
        self.static_keys
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

    pub fn generate_ed25519_static_key(
        &mut self,
        title: impl Into<KeyTitle>,
    ) -> (KeyId, UnsecuredEd25519Key) {
        let title = title.into();
        let secure_key = Ed25519Key::generate(&mut OsRng);
        let unsecured = secure_key.clone().into_unsecured();

        self.set_static_key(title.clone(), Key::Ed25519(secure_key));
        (key_id(&self.static_keys[&title]), unsecured)
    }

    pub fn generate_zk_static_key(
        &mut self,
        title: impl Into<KeyTitle>,
    ) -> (KeyId, UnsecuredZkKey) {
        let title = title.into();
        let secure_key = generate_zk_key_from_random_bytes();
        let unsecured = secure_key.clone().into_unsecured();

        self.set_static_key(title.clone(), Key::Zk(secure_key));
        (key_id(&self.static_keys[&title]), unsecured)
    }

    pub fn remove_static_key(&mut self, title: impl Into<KeyTitle>) -> Option<(KeyId, Key)> {
        let title = title.into();
        self.static_keys.remove(&title).map(|v| (key_id(&v), v))
    }

    #[must_use]
    pub fn new(mnemonic: Mnemonic, passphrase: Option<Passphrase>) -> Self {
        let mut keystore = Self::from_static_keys(mnemonic, passphrase, HashMap::new());

        for title in KeyTitle::PREDEFINED_ED25519 {
            keystore.generate_ed25519_static_key(title);
        }

        for title in KeyTitle::PREDEFINED_ZK {
            keystore.generate_zk_static_key(title);
        }

        keystore
    }

    /// A keystore that holds the static keys given, and no other.
    #[must_use]
    pub fn from_static_keys(
        mnemonic: Mnemonic,
        passphrase: Option<Passphrase>,
        static_keys: HashMap<KeyTitle, Key>,
    ) -> Self {
        Self {
            mnemonic,
            passphrase,
            static_keys,
            warning: WARNING.to_owned(),
        }
    }

    /// The public key of the receive address at the index
    #[must_use]
    pub fn receive_public_key(&self, index: HardenedIndex) -> ZkPublicKey {
        MasterSeed::from_mnemonic(&self.mnemonic, self.passphrase.as_ref())
            .to_key()
            .derive_key(&hd::receive_path(index))
            .to_zk_key()
            .to_public_key()
    }

    /// The address that holds the stake, which is the first receive address
    #[must_use]
    pub fn stake_public_key(&self) -> ZkPublicKey {
        self.receive_public_key(hd::STAKE_RECEIVE_INDEX)
    }

    /// The key that holds the stake in a keystore migrated from v0.3.0
    #[must_use]
    pub fn legacy_stake_public_key(&self) -> Option<ZkPublicKey> {
        self.get_zk_static_key(KeyTitle::LEGACY_STAKE)
            .ok()
            .map(|(_, key)| key.to_public_key())
    }

    /// The keys whose notes are never spent to fund a transaction: the stake
    /// address, and the legacy stake key if any.
    #[must_use]
    pub fn unspendable_public_keys(&self) -> HashSet<ZkPublicKey> {
        std::iter::once(self.stake_public_key())
            .chain(self.legacy_stake_public_key())
            .collect()
    }

    /// The address that the `PoW` rewards are paid to, which is the second
    /// receive address
    #[must_use]
    pub fn pow_claim_public_key(&self) -> ZkPublicKey {
        self.receive_public_key(hd::FUNDING_RECEIVE_INDEX)
    }

    #[must_use]
    pub fn kms_backend_settings(&self) -> KmsBackendSettings {
        KmsBackendSettings {
            mnemonic: self.mnemonic.clone(),
            passphrase: self.passphrase.clone(),
            static_keys: self
                .get_static_keys()
                .map(|(id, key)| (id, key.clone()))
                .collect(),
        }
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
