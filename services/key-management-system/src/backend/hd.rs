//! This module contains an implementation of [`KMSBackend`] for HD wallets,
//! where keys are either preloaded under a name or derived from a master key
//! at a path.
use std::{
    collections::HashMap,
    fmt::{self, Debug, Display},
};

use lb_key_management_system_keys::{
    hd::{MasterKey, MasterSeed, Mnemonic, Path},
    keys::{Key, KeyOperators, errors::KeyError, secured_key::SecuredKey},
};
use serde::{Deserialize, Serialize};

use crate::backend::{
    KMSBackend,
    preload::{self, PreloadBackendError, PreloadKMSBackend, PreloadKMSBackendSettings},
};

/// The id of a key in the [`HdKMSBackend`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KeyId {
    /// A key preloaded under the name.
    Name(preload::KeyId),
    /// A key derived from the master key at the path.
    Path(Path),
}

impl From<preload::KeyId> for KeyId {
    fn from(name: preload::KeyId) -> Self {
        Self::Name(name)
    }
}

impl From<&str> for KeyId {
    fn from(name: &str) -> Self {
        Self::Name(name.to_owned())
    }
}

impl From<Path> for KeyId {
    fn from(path: Path) -> Self {
        Self::Path(path)
    }
}

impl Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name(name) => Display::fmt(name, f),
            Self::Path(path) => Display::fmt(path, f),
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum HdBackendError {
    #[error(transparent)]
    Preload(#[from] PreloadBackendError),
    #[error(transparent)]
    KeyError(#[from] KeyError),
    #[error("key at {0} cannot be derived because the master key is not set")]
    MissingMasterKey(Path),
    #[error("key at {0} cannot be registered because it is derived from the master key")]
    RegisteringDerivedKey(Path),
}

pub struct HdKMSBackend {
    preloaded: PreloadKMSBackend,
    master: Option<MasterKey>,
}

/// The settings of the [`HdKMSBackend`].
#[derive(Clone)]
pub struct HdKMSBackendSettings {
    /// The BIP-39 mnemonic that the keys of [`KeyId::Path`] are derived from.
    pub mnemonic: Option<Mnemonic>,
    /// The BIP-39 passphrase of the mnemonic, which is empty if not set.
    pub passphrase: Option<String>,
    /// The keys of [`KeyId::Name`], by name.
    pub keys: HashMap<preload::KeyId, Key>,
}

impl Debug for HdKMSBackendSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HdKMSBackendSettings")
            .field("mnemonic", &self.mnemonic)
            .field(
                "passphrase",
                &self.passphrase.as_ref().map(|_| "<redacted>"),
            )
            .field("keys", &self.keys)
            .finish()
    }
}

impl HdKMSBackend {
    /// Derives the key at the path from the master key.
    fn derive(&self, path: &Path) -> Result<Key, HdBackendError> {
        let master = self
            .master
            .as_ref()
            .ok_or(HdBackendError::MissingMasterKey(*path))?;
        Ok(Key::Zk(master.derive_key(path).to_zk_key()))
    }
}

#[async_trait::async_trait]
impl KMSBackend for HdKMSBackend {
    type KeyId = KeyId;
    type Key = Key;
    type KeyOperations = KeyOperators;
    type Settings = HdKMSBackendSettings;
    type Error = HdBackendError;

    fn new(settings: Self::Settings) -> Self {
        let HdKMSBackendSettings {
            mnemonic,
            passphrase,
            keys,
        } = settings;
        let passphrase = passphrase.as_deref().unwrap_or_default();
        Self {
            preloaded: PreloadKMSBackend::new(PreloadKMSBackendSettings { keys }),
            master: mnemonic
                .map(|mnemonic| MasterSeed::from_mnemonic(&mnemonic, passphrase).to_key()),
        }
    }

    fn register(&mut self, key_id: &Self::KeyId, key: Self::Key) -> Result<(), Self::Error> {
        match key_id {
            KeyId::Name(name) => Ok(self.preloaded.register(name, key)?),
            KeyId::Path(path) => Err(HdBackendError::RegisteringDerivedKey(*path)),
        }
    }

    fn public_key(
        &self,
        key_id: &Self::KeyId,
    ) -> Result<<Self::Key as SecuredKey>::PublicKey, Self::Error> {
        match key_id {
            KeyId::Name(name) => Ok(self.preloaded.public_key(name)?),
            KeyId::Path(path) => Ok(self.derive(path)?.as_public_key()),
        }
    }

    fn sign(
        &self,
        key_id: &Self::KeyId,
        payload: <Self::Key as SecuredKey>::Payload,
    ) -> Result<<Self::Key as SecuredKey>::Signature, Self::Error> {
        match key_id {
            KeyId::Name(name) => Ok(self.preloaded.sign(name, payload)?),
            KeyId::Path(path) => Ok(self.derive(path)?.sign(&payload)?),
        }
    }

    fn sign_multiple(
        &self,
        key_ids: &[Self::KeyId],
        payload: <Self::Key as SecuredKey>::Payload,
    ) -> Result<<Self::Key as SecuredKey>::Signature, Self::Error> {
        let derived = key_ids
            .iter()
            .map(|key_id| match key_id {
                KeyId::Name(_) => Ok(None),
                KeyId::Path(path) => self.derive(path).map(Some),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let keys = key_ids
            .iter()
            .zip(&derived)
            .map(|(key_id, derived)| match (key_id, derived) {
                (KeyId::Name(name), _) => Ok(self.preloaded.key(name)?),
                (KeyId::Path(_), Some(key)) => Ok(key),
                (KeyId::Path(path), None) => Err(HdBackendError::MissingMasterKey(*path)),
            })
            .collect::<Result<Vec<_>, HdBackendError>>()?;

        Ok(Self::Key::sign_multiple(&keys, &payload)?)
    }

    async fn execute(
        &mut self,
        key_id: &Self::KeyId,
        operator: Self::KeyOperations,
    ) -> Result<(), Self::Error> {
        match key_id {
            KeyId::Name(name) => Ok(self.preloaded.execute(name, operator).await?),
            KeyId::Path(path) => Ok(self.derive(path)?.execute(operator).await?),
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
    fn preloaded_key_is_found_by_name() {
        let key = Key::Ed25519(Ed25519Key::generate(&mut OsRng));
        let backend = backend([("BlendSigning".to_owned(), key.clone())].into());

        assert_eq!(
            backend.public_key(&"BlendSigning".into()).unwrap(),
            key.as_public_key()
        );
        assert!(matches!(
            backend.public_key(&"Unknown".into()),
            Err(HdBackendError::Preload(
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
    fn passphrase_changes_derived_keys() {
        let backend = HdKMSBackend::new(HdKMSBackendSettings {
            mnemonic: Some(MNEMONIC.parse().unwrap()),
            passphrase: Some("passphrase".to_owned()),
            keys: HashMap::new(),
        });

        assert_ne!(
            backend.public_key(&receive(0)).unwrap(),
            receive_0_key().as_public_key()
        );
    }

    #[test]
    fn deriving_requires_master_key() {
        let backend = HdKMSBackend::new(HdKMSBackendSettings {
            mnemonic: None,
            passphrase: None,
            keys: HashMap::new(),
        });

        assert!(matches!(
            backend.public_key(&receive(0)),
            Err(HdBackendError::MissingMasterKey(_))
        ));
    }

    #[test]
    fn derived_key_cannot_be_registered() {
        let mut backend = backend(HashMap::new());

        assert!(matches!(
            backend.register(&receive(0), receive_0_key()),
            Err(HdBackendError::RegisteringDerivedKey(_))
        ));
        backend.register(&"Extra".into(), receive_0_key()).unwrap();
        assert_eq!(
            backend.public_key(&"Extra".into()).unwrap(),
            receive_0_key().as_public_key()
        );
    }

    #[test]
    fn sign_with_derived_key() {
        let backend = backend(HashMap::new());
        let data = Fr::from(7u8);

        let signature = backend
            .sign(&receive(0), PayloadEncoding::Zk(data))
            .unwrap();

        assert!(verify(&[receive_0_key()], &data, &signature));
    }

    #[test]
    fn sign_multiple_with_preloaded_and_derived_keys() {
        let preloaded = Key::Zk(ZkKey::new(BigUint::from_bytes_le(&[1u8; 32]).into()));
        let backend = backend([("BlendZk".to_owned(), preloaded.clone())].into());
        let data = Fr::from(7u8);

        let signature = backend
            .sign_multiple(&["BlendZk".into(), receive(0)], PayloadEncoding::Zk(data))
            .unwrap();

        assert!(verify(&[preloaded, receive_0_key()], &data, &signature));
    }

    #[test]
    fn key_id_is_displayed_as_name_or_path() {
        assert_eq!(KeyId::from("BlendZk").to_string(), "BlendZk");
        assert_eq!(receive(3).to_string(), "m/154'/0'/0'/3'");
    }

    fn backend(keys: HashMap<preload::KeyId, Key>) -> HdKMSBackend {
        HdKMSBackend::new(HdKMSBackendSettings {
            mnemonic: Some(MNEMONIC.parse().unwrap()),
            passphrase: None,
            keys,
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
        let master = MasterSeed::from_mnemonic(&MNEMONIC.parse().unwrap(), "").to_key();
        let path = "m/154'/0'/0'/0'".parse().unwrap();
        Key::Zk(master.derive_key(&path).to_zk_key())
    }
}
