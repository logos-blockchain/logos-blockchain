//! This module contains an implementation of [`KMSBackend`] for HD wallets,
//! where keys are either preloaded under a name or derived from a master key
//! at a path.
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
    #[error("key at {0} cannot be registered because it is derived from the master key")]
    RegisteringDerivedKey(Path),
}

pub struct HdKMSBackend {
    preloaded: PreloadKMSBackend,
    master: MasterKey,
}

/// The settings of the [`HdKMSBackend`].
#[derive(Clone, Debug)]
pub struct HdKMSBackendSettings {
    /// The BIP-39 mnemonic that the keys of [`KeyId::Path`] are derived from.
    pub mnemonic: Mnemonic,
    /// The BIP-39 passphrase of the mnemonic, which is empty if not set.
    pub passphrase: Option<Passphrase>,
    /// The keys of [`KeyId::Name`], by name.
    pub keys: HashMap<preload::KeyId, Key>,
}

#[async_trait::async_trait]
impl KMSBackend for HdKMSBackend {
    type KeyId = KeyId;
    type Key = Key;
    type KeyOperations = KeyOperators;
    type Settings = HdKMSBackendSettings;
    type Error = HdBackendError;

    fn new(settings: Self::Settings) -> Self {
        // TODO(hd_wallet_06_kms)
        todo!()
    }

    fn register(&mut self, key_id: &Self::KeyId, key: Self::Key) -> Result<(), Self::Error> {
        // TODO(hd_wallet_06_kms)
        todo!()
    }

    fn public_key(
        &self,
        key_id: &Self::KeyId,
    ) -> Result<<Self::Key as SecuredKey>::PublicKey, Self::Error> {
        // TODO(hd_wallet_06_kms)
        todo!()
    }

    fn sign(
        &self,
        key_id: &Self::KeyId,
        payload: <Self::Key as SecuredKey>::Payload,
    ) -> Result<<Self::Key as SecuredKey>::Signature, Self::Error> {
        // TODO(hd_wallet_06_kms)
        todo!()
    }

    fn sign_multiple(
        &self,
        key_ids: &[Self::KeyId],
        payload: <Self::Key as SecuredKey>::Payload,
    ) -> Result<<Self::Key as SecuredKey>::Signature, Self::Error> {
        // TODO(hd_wallet_06_kms)
        todo!()
    }

    async fn execute(
        &mut self,
        key_id: &Self::KeyId,
        operator: Self::KeyOperations,
    ) -> Result<(), Self::Error> {
        // TODO(hd_wallet_06_kms)
        todo!()
    }
}
