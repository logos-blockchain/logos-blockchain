//! This module contains an implementation of [`KMSBackend`] where keys are
//! either derived from a HD master key at a HD path,
//! or preloaded from config file.
//!
//! The preloaded keys are held by a [`PreloadKMSBackend`] wrapped in this
//! module.
use std::{
    collections::HashMap,
    fmt::{self, Display},
};

use lb_key_management_system_keys::{
    hd::{MasterKey, MasterSeed, Mnemonic, Passphrase, Path},
    keys::{Key, KeyOperators, errors::KeyError, secured_key::SecuredKey},
};
use serde::{Deserialize, Serialize};

use crate::backend::{
    KMSBackend,
    preload::{self, PreloadBackendError, PreloadKMSBackend, PreloadKMSBackendSettings},
};

/// The id of a key in the [`HdAndPreloadKMSBackend`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KeyId {
    /// A key derived from the master key at the path.
    Hd(Path),
    /// A key preloaded under the id.
    Static(preload::KeyId),
}

impl From<Path> for KeyId {
    fn from(path: Path) -> Self {
        Self::Hd(path)
    }
}

impl From<preload::KeyId> for KeyId {
    fn from(id: preload::KeyId) -> Self {
        Self::Static(id)
    }
}

impl From<&str> for KeyId {
    fn from(id: &str) -> Self {
        Self::Static(id.to_owned())
    }
}

impl Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hd(path) => Display::fmt(path, f),
            Self::Static(id) => Display::fmt(id, f),
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum HdAndPreloadBackendError {
    #[error(transparent)]
    Preload(#[from] PreloadBackendError),
    #[error(transparent)]
    KeyError(#[from] KeyError),
    #[error("key at {0} cannot be registered because it is derived from the master key")]
    RegisteringHdKey(Path),
}

pub struct HdAndPreloadKMSBackend {
    master: MasterKey,
    preloaded: PreloadKMSBackend,
}

/// The settings of the [`HdAndPreloadKMSBackend`].
#[derive(Clone, Debug)]
pub struct HdAndPreloadKMSBackendSettings {
    /// The BIP-39 mnemonic that the keys of [`KeyId::Hd`] are derived from.
    pub mnemonic: Mnemonic,
    /// The BIP-39 passphrase of the mnemonic, which is empty if not set.
    pub passphrase: Option<Passphrase>,
    /// The keys of [`KeyId::Static`], by id.
    pub static_keys: HashMap<preload::KeyId, Key>,
}

impl HdAndPreloadKMSBackend {
    /// Derives the key at the path from the master key.
    fn derive(&self, path: &Path) -> Key {
        Key::Zk(self.master.derive_key(path).to_zk_key())
    }
}

#[async_trait::async_trait]
impl KMSBackend for HdAndPreloadKMSBackend {
    type KeyId = KeyId;
    type Key = Key;
    type KeyOperations = KeyOperators;
    type Settings = HdAndPreloadKMSBackendSettings;
    type Error = HdAndPreloadBackendError;

    fn new(settings: Self::Settings) -> Self {
        let HdAndPreloadKMSBackendSettings {
            mnemonic,
            passphrase,
            static_keys,
        } = settings;
        Self {
            master: MasterSeed::from_mnemonic(&mnemonic, passphrase.as_ref()).to_key(),
            preloaded: PreloadKMSBackend::new(PreloadKMSBackendSettings { keys: static_keys }),
        }
    }

    fn register(&mut self, key_id: &Self::KeyId, key: Self::Key) -> Result<(), Self::Error> {
        match key_id {
            KeyId::Static(id) => Ok(self.preloaded.register(id, key)?),
            KeyId::Hd(path) => Err(HdAndPreloadBackendError::RegisteringHdKey(*path)),
        }
    }

    fn public_key(
        &self,
        key_id: &Self::KeyId,
    ) -> Result<<Self::Key as SecuredKey>::PublicKey, Self::Error> {
        match key_id {
            KeyId::Static(id) => Ok(self.preloaded.public_key(id)?),
            KeyId::Hd(path) => Ok(self.derive(path).as_public_key()),
        }
    }

    fn sign(
        &self,
        key_id: &Self::KeyId,
        payload: <Self::Key as SecuredKey>::Payload,
    ) -> Result<<Self::Key as SecuredKey>::Signature, Self::Error> {
        match key_id {
            KeyId::Static(id) => Ok(self.preloaded.sign(id, payload)?),
            KeyId::Hd(path) => Ok(self.derive(path).sign(&payload)?),
        }
    }

    fn sign_multiple(
        &self,
        key_ids: &[Self::KeyId],
        payload: <Self::Key as SecuredKey>::Payload,
    ) -> Result<<Self::Key as SecuredKey>::Signature, Self::Error> {
        // Derived first, for the keys to outlive the references to them.
        let derived = key_ids
            .iter()
            .map(|key_id| match key_id {
                KeyId::Static(_) => None,
                KeyId::Hd(path) => Some(self.derive(path)),
            })
            .collect::<Vec<_>>();
        let keys = key_ids
            .iter()
            .zip(&derived)
            .map(|(key_id, derived)| match (key_id, derived) {
                (KeyId::Static(id), _) => self.preloaded.key(id),
                (KeyId::Hd(_), Some(key)) => Ok(key),
                (KeyId::Hd(_), None) => unreachable!("Key at a path is derived"),
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self::Key::sign_multiple(&keys, &payload)?)
    }

    async fn execute(
        &mut self,
        key_id: &Self::KeyId,
        operator: Self::KeyOperations,
    ) -> Result<(), Self::Error> {
        match key_id {
            KeyId::Static(id) => Ok(self.preloaded.execute(id, operator).await?),
            KeyId::Hd(path) => Ok(self.derive(path).execute(operator).await?),
        }
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::Fr;
    use lb_key_management_system_keys::keys::{
        Ed25519Key, PayloadEncoding, PublicKeyEncoding, SignatureEncoding, ZkKey, ZkPublicKey,
    };
    use num_bigint::BigUint;
    use rand::rngs::OsRng;

    use super::*;

    const MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[test]
    fn static_key_is_found_by_id() {
        let key = Key::Ed25519(Ed25519Key::generate(&mut OsRng));
        let backend = backend([("BlendSigning".to_owned(), key.clone())].into());

        assert_eq!(
            backend.public_key(&"BlendSigning".into()).unwrap(),
            key.as_public_key()
        );
        assert!(matches!(
            backend.public_key(&"Unknown".into()),
            Err(HdAndPreloadBackendError::Preload(
                PreloadBackendError::NotRegisteredKeyId(_)
            ))
        ));
    }

    #[test]
    fn key_is_derived_at_path() {
        let backend = backend(HashMap::new());

        let expected = receive_0_key().as_public_key();
        assert_eq!(backend.public_key(&receive(0)).unwrap(), expected);
        assert_ne!(backend.public_key(&receive(1)).unwrap(), expected);
    }

    #[test]
    fn hd_key_cannot_be_registered() {
        let mut backend = backend(HashMap::new());

        assert!(matches!(
            backend.register(&receive(0), receive_0_key()),
            Err(HdAndPreloadBackendError::RegisteringHdKey(_))
        ));
        backend.register(&"Extra".into(), receive_0_key()).unwrap();
        assert_eq!(
            backend.public_key(&"Extra".into()).unwrap(),
            receive_0_key().as_public_key()
        );
    }

    #[test]
    fn sign_with_hd_key() {
        let backend = backend(HashMap::new());
        let data = Fr::from(7u8);

        let signature = backend
            .sign(&receive(0), PayloadEncoding::Zk(data))
            .unwrap();

        assert!(verify(&[receive_0_key()], &data, &signature));
    }

    #[test]
    fn sign_multiple_with_static_and_hd_keys() {
        let preloaded = Key::Zk(ZkKey::new(BigUint::from_bytes_le(&[1u8; 32]).into()));
        let backend = backend([("BlendZk".to_owned(), preloaded.clone())].into());
        let data = Fr::from(7u8);

        let signature = backend
            .sign_multiple(&["BlendZk".into(), receive(0)], PayloadEncoding::Zk(data))
            .unwrap();

        assert!(verify(&[preloaded, receive_0_key()], &data, &signature));
    }

    #[test]
    fn key_id_is_displayed_as_path_or_id() {
        assert_eq!(KeyId::from("BlendZk").to_string(), "BlendZk");
        assert_eq!(receive(3).to_string(), "m/154'/0'/0'/3'");
    }

    #[test]
    fn key_id_is_tagged_in_yaml() {
        let hd: KeyId = serde_yaml::from_str("!Hd m/154'/0'/2'").unwrap();
        assert_eq!(hd, KeyId::Hd("m/154'/0'/2'".parse().unwrap()));
        assert_eq!(serde_yaml::to_string(&hd).unwrap(), "!Hd m/154'/0'/2'\n");

        let static_key: KeyId = serde_yaml::from_str("!Static aa70").unwrap();
        assert_eq!(static_key, KeyId::from("aa70"));
        assert_eq!(
            serde_yaml::to_string(&static_key).unwrap(),
            "!Static aa70\n"
        );

        // An id without a tag is rejected, for an HD path not to be read as
        // the id of a static key.
        serde_yaml::from_str::<KeyId>("aa70").unwrap_err();
    }

    fn backend(static_keys: HashMap<preload::KeyId, Key>) -> HdAndPreloadKMSBackend {
        HdAndPreloadKMSBackend::new(HdAndPreloadKMSBackendSettings {
            mnemonic: MNEMONIC.parse().unwrap(),
            passphrase: None,
            static_keys,
        })
    }

    fn receive(index: u32) -> KeyId {
        format!("m/154'/0'/0'/{index}'")
            .parse::<Path>()
            .unwrap()
            .into()
    }

    fn verify(keys: &[Key], data: &Fr, signature: &SignatureEncoding) -> bool {
        let public_keys: Vec<ZkPublicKey> = keys
            .iter()
            .map(|key| match key.as_public_key() {
                PublicKeyEncoding::Zk(public_key) => public_key,
                PublicKeyEncoding::Ed25519(_) => panic!("expected a ZK key"),
            })
            .collect();
        let SignatureEncoding::Zk(signature) = signature else {
            panic!("expected a ZK signature");
        };
        ZkPublicKey::verify_multi(&public_keys, data, signature)
    }

    /// The key at `m/154'/0'/0'/0'`, derived without the backend.
    fn receive_0_key() -> Key {
        let master = MasterSeed::from_mnemonic(&MNEMONIC.parse().unwrap(), None).to_key();
        let path = "m/154'/0'/0'/0'".parse().unwrap();
        Key::Zk(master.derive_key(&path).to_zk_key())
    }
}
