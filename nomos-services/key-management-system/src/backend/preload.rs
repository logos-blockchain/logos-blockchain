//! This module contains a simple implementation of [`KMSBackend`] where keys
//! are preloaded from config file.
use std::collections::HashMap;

use key_management_system_keys::keys::{
    Key, KeyOperators, errors::KeyError, secured_key::SecuredKey,
};
use serde::Deserialize;

use crate::backend::KMSBackend;

pub type KeyId = String;

#[derive(thiserror::Error, Debug)]
pub enum PreloadBackendError {
    #[error(transparent)]
    KeyError(#[from] KeyError),
    #[error("KeyId ({0:?}) is not registered")]
    NotRegisteredKeyId(KeyId),
    #[error("KeyId {0} is already registered")]
    AlreadyRegisteredKeyId(KeyId),
}

pub struct PreloadKMSBackend {
    keys: HashMap<KeyId, Key>,
}

/// This setting contains all [`Key`]s to be loaded into the
/// [`PreloadKMSBackend`]. This implements [`serde::Serialize`] for users to
/// populate the settings from bytes.
#[derive(Deserialize, Clone, Debug)]
#[cfg_attr(any(test, feature = "unsafe"), derive(serde::Serialize))]
pub struct PreloadKMSBackendSettings {
    pub keys: HashMap<KeyId, Key>,
}

#[async_trait::async_trait]
impl KMSBackend for PreloadKMSBackend {
    type KeyId = KeyId;
    type Key = Key;
    type KeyOperations = KeyOperators;
    type Settings = PreloadKMSBackendSettings;
    type Error = PreloadBackendError;

    fn new(settings: Self::Settings) -> Self {
        Self {
            keys: settings.keys,
        }
    }

    // Keys created after initialization will be held in memory but not persisted
    // across restarts
    fn register(&mut self, key_id: &Self::KeyId, key: Self::Key) -> Result<(), Self::Error> {
        if self.keys.contains_key(key_id) {
            return Err(PreloadBackendError::AlreadyRegisteredKeyId(key_id.clone()));
        }
        self.keys.insert(key_id.clone(), key);

        Ok(())
    }

    fn public_key(
        &self,
        key_id: &Self::KeyId,
    ) -> Result<<Self::Key as SecuredKey>::PublicKey, Self::Error> {
        Ok(self
            .keys
            .get(key_id)
            .ok_or_else(|| PreloadBackendError::NotRegisteredKeyId(key_id.to_owned()))?
            .as_public_key())
    }

    fn sign(
        &self,
        key_id: &Self::KeyId,
        payload: <Self::Key as SecuredKey>::Payload,
    ) -> Result<<Self::Key as SecuredKey>::Signature, Self::Error> {
        Ok(self
            .keys
            .get(key_id)
            .ok_or_else(|| PreloadBackendError::NotRegisteredKeyId(key_id.to_owned()))?
            .sign(&payload)?)
    }

    fn sign_multiple(
        &self,
        key_ids: &[Self::KeyId],
        payload: <Self::Key as SecuredKey>::Payload,
    ) -> Result<<Self::Key as SecuredKey>::Signature, Self::Error> {
        let keys = key_ids
            .iter()
            .map(|key_id| {
                self.keys
                    .get(key_id)
                    .ok_or_else(|| PreloadBackendError::NotRegisteredKeyId(key_id.to_owned()))
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self::Key::sign_multiple(&keys, &payload)?)
    }

    async fn execute(
        &mut self,
        key_id: &Self::KeyId,
        operator: Self::KeyOperations,
    ) -> Result<(), Self::Error> {
        let key = self
            .keys
            .get_mut(key_id)
            .ok_or_else(|| PreloadBackendError::NotRegisteredKeyId(key_id.to_owned()))?;

        key.execute(operator)
            .await
            .map_err(PreloadBackendError::KeyError)
    }
}

#[cfg(test)]
mod tests {
    use std::marker::PhantomData;

    use bytes::{Bytes as RawBytes, Bytes};
    use key_management_system_keys::keys::{
        Ed25519Key, PayloadEncoding, ZkKey, secured_key::SecureKeyOperator,
    };
    use num_bigint::BigUint;
    use rand::rngs::OsRng;

    use super::*;

    #[derive(Debug, PartialEq, Eq, Clone, Copy)]
    pub struct NoKeyOperator<Key, Error> {
        _key: PhantomData<Key>,
        _error: PhantomData<Error>,
    }

    #[async_trait::async_trait]
    impl<Key, Error> SecureKeyOperator for NoKeyOperator<Key, Error>
    where
        Key: Send + Sync + 'static,
        Error: Send + Sync + 'static,
    {
        type Key = Key;
        type Error = Error;

        async fn execute(self: Box<Self>, _key: &Self::Key) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    impl<Key, Error> Default for NoKeyOperator<Key, Error> {
        fn default() -> Self {
            Self::new()
        }
    }

    impl<Key, Error> NoKeyOperator<Key, Error> {
        #[must_use]
        pub const fn new() -> Self {
            Self {
                _key: PhantomData,
                _error: PhantomData,
            }
        }
    }

    #[tokio::test]
    async fn preload_backend() {
        // Initialize a backend with a pre-generated key in the setting
        let key_id = "blend/1".to_owned();
        let key = Key::Ed25519(Ed25519Key::generate(&mut OsRng));
        let mut backend = PreloadKMSBackend::new(PreloadKMSBackendSettings {
            keys: HashMap::from_iter([(key_id.clone(), key.clone())]),
        });

        // Check if the key was preloaded successfully with the same key type.
        assert!(matches!(
            backend.register(&key_id, key.clone()).unwrap_err(),
            PreloadBackendError::AlreadyRegisteredKeyId(id) if id == key_id.clone()
        ));

        let public_key = key.as_public_key();
        let backend_public_key = backend.public_key(&key_id).unwrap();
        assert_eq!(backend_public_key, public_key);

        let payload = PayloadEncoding::Ed25519(Bytes::from("data"));
        let signature = key.sign(&payload).unwrap();
        let backend_signature = backend.sign(&key_id, payload).unwrap();
        assert_eq!(backend_signature, signature);

        // Check if the execute function works as expected
        backend
            .execute(
                &key_id,
                KeyOperators::Ed25519(Box::new(NoKeyOperator::new())),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn key_not_registered() {
        let mut backend = PreloadKMSBackend::new(PreloadKMSBackendSettings {
            keys: HashMap::new(),
        });

        let key_id = "blend/not_registered".to_owned();
        let key = Key::Ed25519(Ed25519Key::generate(&mut OsRng));

        // Fetching public key fails
        assert!(matches!(
            backend.public_key(&key_id).unwrap_err(),
            PreloadBackendError::NotRegisteredKeyId(id) if id == key_id.clone()
        ));

        // Signing with a key id fails
        let data = RawBytes::from("data");
        let encoded_data = PayloadEncoding::Ed25519(data);
        assert!(matches!(
            backend.sign(&key_id, encoded_data).unwrap_err(),
            PreloadBackendError::NotRegisteredKeyId(id) if id == key_id.clone()
        ));

        // Excuting with a key id fails
        assert!(matches!(
            backend
                .execute(&key_id, KeyOperators::Ed25519(Box::new(NoKeyOperator::new())))
                .await
                .unwrap_err(),
            PreloadBackendError::NotRegisteredKeyId(id) if id == key_id.clone()
        ));

        // Registering the key works
        assert!(matches!(backend.register(&key_id, key), Ok(())));
    }

    #[test]
    fn serde_keys_from_yaml() {
        let preloaded_keys = PreloadKMSBackendSettings {
            keys: [
                (
                    "test1".into(),
                    Key::Ed25519(Ed25519Key::generate(&mut OsRng)),
                ),
                (
                    "test2".into(),
                    Key::Zk(ZkKey::new(BigUint::from_bytes_le(&[1u8; 32]).into())),
                ),
            ]
            .into(),
        };

        let mut serialized_ouput = Vec::new();
        serde_yaml::to_writer(&mut serialized_ouput, &preloaded_keys).unwrap();

        let deserialized_keys: PreloadKMSBackendSettings =
            serde_yaml::from_slice(&serialized_ouput).unwrap();

        assert_eq!(preloaded_keys.keys.len(), deserialized_keys.keys.len());
        let original_key = preloaded_keys.keys.keys().next().unwrap();
        assert!(deserialized_keys.keys.contains_key(original_key));
    }
}
