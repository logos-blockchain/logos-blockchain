use std::fmt::Debug;

use zeroize::ZeroizeOnDrop;

#[async_trait::async_trait]
pub trait SecureKeyOperator {
    type Key;
    type Error;
    async fn execute(self: Box<Self>, key: &Self::Key) -> Result<(), Self::Error>;
}

pub trait DebugSecureKeyOperator: SecureKeyOperator + Debug {}
impl<T: SecureKeyOperator + Debug> DebugSecureKeyOperator for T {}

pub type BoxedSecureKeyOperator<Key> =
    Box<dyn DebugSecureKeyOperator<Key = Key, Error = <Key as SecuredKey>::Error> + Send + Sync>;

/// An operation over an ordered collection of independently secured keys.
#[async_trait::async_trait]
pub trait SecureMultiKeyOperator: Debug {
    type Key;
    type Error;

    /// Validate the number of keys before the operation is invoked.
    fn validate_key_count(&self, count: usize) -> Result<(), Self::Error>;

    async fn execute(self: Box<Self>, keys: &[&Self::Key]) -> Result<(), Self::Error>;
}

pub type BoxedSecureMultiKeyOperator<Key> =
    Box<dyn SecureMultiKeyOperator<Key = Key, Error = <Key as SecuredKey>::Error> + Send + Sync>;

/// A key that can be used within the Key Management Service.
#[async_trait::async_trait]
pub trait SecuredKey: ZeroizeOnDrop {
    type Payload;
    type Signature;
    type PublicKey;
    type Error;

    fn sign(&self, payload: &Self::Payload) -> Result<Self::Signature, Self::Error>;
    fn sign_multiple(
        keys: &[&Self],
        payload: &Self::Payload,
    ) -> Result<Self::Signature, Self::Error>
    where
        Self: Sized;
    fn as_public_key(&self) -> Self::PublicKey;

    async fn execute<Operation>(&self, operator: Operation) -> Result<(), Self::Error>
    where
        Operation: SecureKeyOperator<Key = Self, Error = Self::Error> + Send + Debug,
    {
        Box::new(operator).execute(self).await
    }

    async fn execute_multiple<Operation>(
        keys: &[&Self],
        operator: Operation,
    ) -> Result<(), Self::Error>
    where
        Self: Sized + Sync,
        Operation: SecureMultiKeyOperator<Key = Self, Error = Self::Error> + Send,
    {
        operator.validate_key_count(keys.len())?;
        Box::new(operator).execute(keys).await
    }
}
