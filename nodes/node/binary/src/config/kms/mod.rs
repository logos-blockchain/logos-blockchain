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
    type Error = MissingMnemonicError;

    fn try_from(value: ServiceConfig) -> Result<Self, Self::Error> {
        Ok(Self {
            keys: value.user.backend.resolve_keys()?,
        })
    }
}

impl PreloadKmsBackendSettings {
    /// Resolves every [`KeyEntry`] into [`Key`] to load into the KMS.
    pub fn resolve_keys(self) -> Result<HashMap<KeyId, Key>, MissingMnemonicError> {
        let master = self.master_key();
        self.keys
            .iter()
            .map(|(key_id, entry)| {
                let key = entry
                    .resolve(master.as_ref())
                    .ok_or_else(|| MissingMnemonicError(key_id.clone()))?;
                Ok((key_id.clone(), key))
            })
            .collect()
    }

    /// Resolves the [`KeyEntry`] registered under [`KeyId`], if any.
    pub fn resolve_key(&self, key_id: &KeyId) -> Result<Option<Key>, MissingMnemonicError> {
        let Some(entry) = self.keys.get(key_id) else {
            return Ok(None);
        };
        let key = entry
            .resolve(self.master_key().as_ref())
            .ok_or_else(|| MissingMnemonicError(key_id.clone()))?;
        Ok(Some(key))
    }

    /// Derives the [`MasterKey`] from the mnemonic specified, if any.
    fn master_key(&self) -> Option<MasterKey> {
        let mnemonic = self.mnemonic.as_ref()?;
        let passphrase = self.passphrase.as_deref().unwrap_or_default();
        Some(MasterSeed::from_mnemonic(mnemonic, passphrase).to_key())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("key `{0}` is derived from the mnemonic, but the mnemonic is not set")]
pub struct MissingMnemonicError(KeyId);

impl KeyEntry {
    /// Resolves the [`KeyEntry`] into its [`Key`].
    #[must_use]
    pub fn resolve(&self, master: Option<&MasterKey>) -> Option<Key> {
        Some(match self {
            Self::Ed25519(key) => Key::Ed25519(key.clone()),
            Self::Zk(key) => Key::Zk(key.clone()),
            Self::Hd(path) => Key::Zk(master?.derive_key(path).to_zk_key()),
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
            passphrase: None,
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
    fn hd_key_is_derived_with_passphrase() {
        let settings = PreloadKmsBackendSettings {
            mnemonic: Some(MNEMONIC.parse().unwrap()),
            passphrase: Some("passphrase".to_owned()),
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
        assert_ne!(hex::encode(fr_to_bytes(key.as_fr())), RECEIVE_0_ZK_KEY);
    }

    #[test]
    fn hd_key_requires_mnemonic() {
        let settings = PreloadKmsBackendSettings {
            mnemonic: None,
            passphrase: None,
            keys: [(
                "LeaderFunding".to_owned(),
                KeyEntry::Hd("m/154'/0'/0'/0'".parse().unwrap()),
            )]
            .into(),
        };

        assert!(matches!(
            settings.resolve_keys(),
            Err(MissingMnemonicError(key_id)) if key_id == "LeaderFunding"
        ));
    }
}
