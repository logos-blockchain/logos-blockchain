use lb_key_management_system_service::{
    backend::preload::PreloadKMSBackendSettings,
    hd::{MasterKey, MasterSeed},
};

use crate::config::kms::serde::{Config, KmsBackendSettings};

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl From<ServiceConfig> for PreloadKMSBackendSettings {
    fn from(value: ServiceConfig) -> Self {
        // TODO(hd_wallet_06_kms): The KMS takes the mnemonic to derive keys from.
        Self {
            keys: value.user.backend.keys,
        }
    }
}

impl KmsBackendSettings {
    /// Derives the [`MasterKey`] from the mnemonic.
    #[must_use]
    pub fn master_key(&self) -> MasterKey {
        let passphrase = self.passphrase.as_deref().unwrap_or_default();
        MasterSeed::from_mnemonic(&self.mnemonic, passphrase).to_key()
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::fr_to_bytes;
    use lb_key_management_system_service::hd::Passphrase;

    use super::*;

    // Test vectors of the spec
    const MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    // The ZK key at m/154'/0'/0'/0'
    const RECEIVE_0_ZK_KEY: &str =
        "5e09bf4ce6b3f42970104a6f5940104407f98da0eb946104c13fb4f94c011f16";

    #[test]
    fn master_key_is_derived_from_mnemonic() {
        assert_eq!(receive_0_key(None), RECEIVE_0_ZK_KEY);
    }

    #[test]
    fn master_key_is_derived_with_passphrase() {
        assert_ne!(receive_0_key(Some("passphrase".into())), RECEIVE_0_ZK_KEY);
    }

    fn receive_0_key(passphrase: Option<Passphrase>) -> String {
        let settings = KmsBackendSettings {
            mnemonic: MNEMONIC.parse().unwrap(),
            passphrase,
            keys: [].into(),
        };
        let key = settings
            .master_key()
            .derive_key(&"m/154'/0'/0'/0'".parse().unwrap())
            .to_zk_key();
        hex::encode(fr_to_bytes(key.as_fr()))
    }
}
