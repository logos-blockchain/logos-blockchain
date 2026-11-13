use std::{collections::HashMap, fs::File, io::Write as _, path::Path};

use color_eyre::eyre::{Result, bail, eyre};
use lb_groth16::fr_to_bytes;
use lb_key_management_system_service::{
    backend::preload::KeyId,
    keys::{
        Ed25519Key, Key, UnsecuredEd25519Key, UnsecuredZkKey, ZkKey, secured_key::SecuredKey as _,
    },
};
use lb_utils::yaml::merge;
use num_bigint::BigUint;
use rand::rngs::OsRng;
use rustix::fs::{Mode, OFlags};
use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value, to_value};
use thiserror::Error;

use crate::config::pow::serde::{ClaimTarget, default_claim_threshold};

const WARNING: &str = "Do not share your secret keys";

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
    pub const VAUCHER_MASTER: &str = "VaucherMaster";
    pub const STAKE: &str = "Stake";

    pub const PREDEFINED_ED25519: [&'static str; 2] = [Self::BLEND_SIGNING, Self::NETWORK_SWARM];
    pub const PREDEFINED_ZK: [&'static str; 6] = [
        Self::BLEND_ZK,
        Self::LEADER_FUNDING,
        Self::POW_CLAIM,
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
    /// Reads the keystore at `path`, refusing it when its group or others can
    /// access it.
    pub fn load(path: &Path) -> Result<Self> {
        let read_error =
            |error: std::io::Error| eyre!("Failed to read keystore {}: {error}", path.display());

        let file = File::open(path).map_err(read_error)?;
        let stat = rustix::fs::fstat(&file).map_err(|errno| read_error(errno.into()))?;
        let mode = Mode::from_raw_mode(stat.st_mode);
        if mode.intersects(Mode::RWXG | Mode::RWXO) {
            bail!(
                "Other users can access keystore {path} (mode {mode:04o}). Restrict it to its \
                 owner with `chmod 600 '{path}'`.",
                path = path.display()
            );
        }
        let yaml = std::io::read_to_string(&file).map_err(read_error)?;
        Ok(serde_yaml::from_str(&yaml)?)
    }

    /// Writes the keystore to `path`. A new keystore is created for its owner
    /// only (mode 600). An existing one keeps its mode, which [`Self::load`]
    /// refuses unless only its owner can access it.
    pub fn save(&self, path: &Path) -> Result<()> {
        let yaml = serde_yaml::to_string(self)?;
        let file = rustix::fs::open(
            path,
            OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )?;
        File::from(file).write_all(yaml.as_bytes())?;
        Ok(())
    }

    /// The user config values derived from the predefined keys, as a sparse
    /// config mapping: the node identity, the key ids and public keys the
    /// services need, and every key for the KMS.
    pub fn config_values(&self) -> Result<Value> {
        let (_, node_key) = self.get_ed25519(KeyTitle::NETWORK_SWARM)?;
        let (blend_signing_key_id, _) = self.get_required(KeyTitle::BLEND_SIGNING)?;
        let (blend_zk_key_id, _) = self.get_required(KeyTitle::BLEND_ZK)?;
        let (_, leader_funding_key) = self.get_zk(KeyTitle::LEADER_FUNDING)?;
        let (_, sdp_funding_key) = self.get_zk(KeyTitle::SDP_FUNDING)?;
        let (voucher_master_key_id, _) = self.get_required(KeyTitle::VAUCHER_MASTER)?;
        let (_, pow_claim_key) = self.get_zk(KeyTitle::POW_CLAIM)?;

        let values = [
            (
                "network.backend.swarm.node_key",
                to_value(hex::encode(node_key.as_bytes()))?,
            ),
            (
                "blend.non_ephemeral_signing_key_id",
                to_value(blend_signing_key_id)?,
            ),
            (
                "blend.core.zk.secret_key_kms_id",
                to_value(blend_zk_key_id)?,
            ),
            (
                "cryptarchia.leader.wallet.funding_pk",
                to_value(leader_funding_key.to_public_key())?,
            ),
            (
                "sdp.wallet.funding_pk",
                to_value(sdp_funding_key.to_public_key())?,
            ),
            (
                "wallet.voucher_master_key_id",
                to_value(voucher_master_key_id)?,
            ),
            (
                "wallet.known_keys",
                to_value(
                    self.get_all_zk()
                        .map(|(id, key)| (id, key.to_public_key()))
                        .collect::<HashMap<_, _>>(),
                )?,
            ),
            (
                "kms.backend.keys",
                to_value(self.get_all().collect::<HashMap<_, _>>())?,
            ),
            (
                "pow.auto_claim.targets",
                to_value([ClaimTarget {
                    public_key: pow_claim_key.to_public_key(),
                    threshold: default_claim_threshold(),
                }])?,
            ),
        ];

        let mut config = Value::Mapping(Mapping::new());
        for (path, value) in values {
            merge(&mut config, nested(path, value));
        }
        Ok(config)
    }

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

    fn get_required(&self, title: &str) -> Result<(KeyId, &Key), KeystoreError> {
        self.get(title)
            .ok_or_else(|| KeystoreError::NotFound(title.into()))
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

fn nested(path: &str, value: Value) -> Value {
    path.rsplit('.').fold(value, |value, key| {
        Value::Mapping(Mapping::from_iter([(key.into(), value)]))
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_writes_a_keystore_only_its_owner_can_access() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keystore.yaml");

        Keystore::default().save(&path).unwrap();

        let mode = Mode::from_raw_mode(rustix::fs::stat(&path).unwrap().st_mode);
        assert_eq!(mode, Mode::RUSR | Mode::WUSR);
        Keystore::load(&path).unwrap();
    }

    #[test]
    fn load_refuses_a_keystore_its_group_or_others_can_access() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keystore.yaml");
        Keystore::default().save(&path).unwrap();

        for other in [
            Mode::RGRP,
            Mode::WGRP,
            Mode::XGRP,
            Mode::ROTH,
            Mode::WOTH,
            Mode::XOTH,
        ] {
            let mode = Mode::RUSR | Mode::WUSR | other;
            rustix::fs::chmod(&path, mode).unwrap();
            let Err(error) = Keystore::load(&path) else {
                panic!("a keystore of mode {mode:04o} was accepted");
            };
            assert!(
                error.to_string().contains(&format!("(mode {mode:04o})")),
                "{error}"
            );
        }

        rustix::fs::chmod(&path, Mode::RUSR).unwrap();
        Keystore::load(&path).unwrap();
    }
}
