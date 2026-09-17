use std::collections::HashMap;

use lb_key_management_system_service::{
    backend::preload::{KeyId, PreloadKMSBackendSettings},
    hd::{MasterKey, MasterSeed},
    keys::Key,
};

use crate::config::kms::serde::{Config, KeyEntry, PreloadKmsBackendSettings};

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl TryFrom<ServiceConfig> for PreloadKMSBackendSettings {
    type Error = KmsConfigError;

    fn try_from(value: ServiceConfig) -> Result<Self, Self::Error> {
        Ok(Self {
            keys: value.user.backend.resolve_keys()?,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum KmsConfigError {
    #[error("key `{0}` is derived from the mnemonic, but `kms.backend.mnemonic` is not set")]
    MissingMnemonic(KeyId),
}

impl PreloadKmsBackendSettings {
    /// Resolves every entry into the key to load into the KMS.
    pub fn resolve_keys(&self) -> Result<HashMap<KeyId, Key>, KmsConfigError> {
        let master = self.master_key();
        self.keys
            .iter()
            .map(|(key_id, entry)| {
                let key = entry
                    .resolve(master.as_ref())
                    .ok_or_else(|| KmsConfigError::MissingMnemonic(key_id.clone()))?;
                Ok((key_id.clone(), key))
            })
            .collect()
    }

    /// Resolves the entry registered under `key_id`, if any.
    pub fn resolve_key(&self, key_id: &KeyId) -> Result<Option<Key>, KmsConfigError> {
        let Some(entry) = self.keys.get(key_id) else {
            return Ok(None);
        };
        let key = entry
            .resolve(self.master_key().as_ref())
            .ok_or_else(|| KmsConfigError::MissingMnemonic(key_id.clone()))?;
        Ok(Some(key))
    }

    fn master_key(&self) -> Option<MasterKey> {
        let mnemonic = self.mnemonic.as_ref()?;
        Some(MasterSeed::from_mnemonic(mnemonic, "").to_key())
    }
}

impl KeyEntry {
    /// Resolves the entry into its key, or `None` if it is derived from the
    /// master key but none is given.
    #[must_use]
    pub fn resolve(&self, master: Option<&MasterKey>) -> Option<Key> {
        Some(match self {
            Self::Ed25519(key) => Key::Ed25519(key.clone()),
            Self::Zk(key) => Key::Zk(key.clone()),
            Self::Hd(path) => Key::Zk(master?.derive_leaf(path).to_zk_key()),
        })
    }
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
    fn hd_key_is_derived_from_mnemonic() {
        let settings = PreloadKmsBackendSettings {
            mnemonic: Some(MNEMONIC.parse().unwrap()),
            keys: [(
                "LeaderFunding".to_owned(),
                KeyEntry::Hd("m/154'/0'/0'/0'".parse().unwrap()),
            )]
            .into(),
        };

        let keys = settings.resolve_keys().unwrap();
        let Some(Key::Zk(key)) = keys.get("LeaderFunding") else {
            panic!("expected a ZK key");
        };
        assert_eq!(hex::encode(fr_to_bytes(key.as_fr())), RECEIVE_0_ZK_KEY);
    }

    #[test]
    fn hd_key_requires_mnemonic() {
        let settings = PreloadKmsBackendSettings {
            mnemonic: None,
            keys: [(
                "LeaderFunding".to_owned(),
                KeyEntry::Hd("m/154'/0'/0'/0'".parse().unwrap()),
            )]
            .into(),
        };

        assert!(matches!(
            settings.resolve_keys(),
            Err(KmsConfigError::MissingMnemonic(key_id)) if key_id == "LeaderFunding"
        ));
    }
}
