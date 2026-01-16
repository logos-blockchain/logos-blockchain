use key_management_system_keys::keys::secured_key::{SecureKeyOperator, SecuredKey};

pub mod preload;

#[async_trait::async_trait]
pub trait KMSBackend {
    type KeyId;
    type Key: SecuredKey;
    type KeyOperations: SecureKeyOperator<Key = Self::Key, Error = <Self::Key as SecuredKey>::Error>;
    type Settings;
    type Error;

    fn new(settings: Self::Settings) -> Self;

    fn register(&mut self, key_id: &Self::KeyId, key: Self::Key) -> Result<(), Self::Error>;

    fn public_key(
        &self,
        key_id: &Self::KeyId,
    ) -> Result<<Self::Key as SecuredKey>::PublicKey, Self::Error>;

    fn sign(
        &self,
        key_id: &Self::KeyId,
        payload: <Self::Key as SecuredKey>::Payload,
    ) -> Result<<Self::Key as SecuredKey>::Signature, Self::Error>;

    fn sign_multiple(
        &self,
        key_ids: &[Self::KeyId],
        payload: <Self::Key as SecuredKey>::Payload,
    ) -> Result<<Self::Key as SecuredKey>::Signature, Self::Error>;

    async fn execute(
        &mut self,
        key_id: &Self::KeyId,
        operator: Self::KeyOperations,
    ) -> Result<(), Self::Error>;
}
