use std::collections::HashMap;

use lb_key_management_system_service::{
    backend::{hd::HdKMSBackendSettings, preload::KeyId},
    hd::{MasterKey, MasterSeed},
    keys::Key,
};

use crate::config::kms::serde::{Config, KeyEntry, PreloadKmsBackendSettings};

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl From<ServiceConfig> for HdKMSBackendSettings {
    fn from(value: ServiceConfig) -> Self {
        let backend = value.user.backend;
        Self {
            mnemonic: backend.mnemonic.clone(),
            passphrase: backend.passphrase.clone(),
            keys: backend.resolve_keys(),
        }
    }
}

impl PreloadKmsBackendSettings {
    /// Resolves every [`KeyEntry`] into [`Key`] to load into the KMS.
    #[must_use]
    pub fn resolve_keys(self) -> HashMap<KeyId, Key> {
        let master = self.master_key();
        self.keys
            .iter()
            .map(|(key_id, entry)| (key_id.clone(), entry.resolve(&master)))
            .collect()
    }

    /// Resolves the [`KeyEntry`] registered under [`KeyId`], if any.
    #[must_use]
    pub fn resolve_key(&self, key_id: &KeyId) -> Option<Key> {
        let entry = self.keys.get(key_id)?;
        Some(entry.resolve(&self.master_key()))
    }

    /// Derives the [`MasterKey`] from the mnemonic.
    #[must_use]
    pub fn master_key(&self) -> MasterKey {
        let passphrase = self.passphrase.as_deref().unwrap_or_default();
        MasterSeed::from_mnemonic(&self.mnemonic, passphrase).to_key()
    }
}

impl KeyEntry {
    /// Resolves the [`KeyEntry`] into its [`Key`].
    #[must_use]
    pub fn resolve(&self, master: &MasterKey) -> Key {
        match self {
            Self::Ed25519(key) => Key::Ed25519(key.clone()),
            Self::Zk(key) => Key::Zk(key.clone()),
            Self::Hd(path) => Key::Zk(master.derive_key(path).to_zk_key()),
        }
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
            mnemonic: MNEMONIC.parse().unwrap(),
            passphrase: None,
            keys: [(
                "LeaderFunding".to_owned(),
                KeyEntry::Hd("m/154'/0'/0'/0'".parse().unwrap()),
            )]
            .into(),
        };

        let keys = settings.resolve_keys();
        let Some(Key::Zk(key)) = keys.get("LeaderFunding") else {
            panic!("expected a ZK key");
        };
        assert_eq!(hex::encode(fr_to_bytes(key.as_fr())), RECEIVE_0_ZK_KEY);
    }

    #[test]
    fn hd_key_is_derived_with_passphrase() {
        let settings = PreloadKmsBackendSettings {
            mnemonic: MNEMONIC.parse().unwrap(),
            passphrase: Some("passphrase".to_owned()),
            keys: [(
                "LeaderFunding".to_owned(),
                KeyEntry::Hd("m/154'/0'/0'/0'".parse().unwrap()),
            )]
            .into(),
        };

        let keys = settings.resolve_keys();
        let Some(Key::Zk(key)) = keys.get("LeaderFunding") else {
            panic!("expected a ZK key");
        };
        assert_ne!(hex::encode(fr_to_bytes(key.as_fr())), RECEIVE_0_ZK_KEY);
    }
}
