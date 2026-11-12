pub mod errors;

mod ed25519;
mod zk;

use std::fmt::Debug;

use lb_key_management_system_macros::KmsEnumKey;
#[cfg(feature = "prove")]
use lb_key_management_system_macros::KmsEnumSigningKey;
use serde::Deserialize;
use zeroize::ZeroizeOnDrop;

pub use crate::keys::{
    ed25519::{
        ED25519_PUBLIC_KEY_SIZE, ED25519_SECRET_KEY_SIZE, ED25519_SIGNATURE_SIZE, Ed25519Key,
        PublicKey as Ed25519PublicKey, SharedKey, Signature as Ed25519Signature,
        UnsecuredEd25519Key, UnverifiedPublicKey as UnverifiedEd25519PublicKey,
        UnverifiedX25519PublicKey, X25519PrivateKey,
    },
    zk::{
        MAX_ZK_SIGNING_KEYS, PublicKey as ZkPublicKey, PublicKeys as ZkPublicKeys,
        Signature as ZkSignature, UnsecuredZkKey, ZkKey, public_inputs_from_pks,
    },
};

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

/// Key material and custom operations available independently of signing.
///
/// The associated payload and signature types describe the key's formats;
/// producing signatures additionally requires [`SigningKey`].
#[async_trait::async_trait]
pub trait SecuredKey: ZeroizeOnDrop {
    type Payload;
    type Signature;
    type PublicKey;
    type Error;

    fn as_public_key(&self) -> Self::PublicKey;

    async fn execute<Operation>(&self, operator: Operation) -> Result<(), Self::Error>
    where
        Operation: SecureKeyOperator<Key = Self, Error = Self::Error> + Send + Debug,
    {
        Box::new(operator).execute(self).await
    }
}

/// A secured key whose signing implementation is available in this build.
///
/// Ed25519 keys always implement this trait. ZK keys and the mixed `Key` enum
/// implement it only when the `prove` feature is enabled.
pub trait SigningKey: SecuredKey {
    fn sign(&self, payload: &Self::Payload) -> Result<Self::Signature, Self::Error>;
    fn sign_multiple(
        keys: &[&Self],
        payload: &Self::Payload,
    ) -> Result<Self::Signature, Self::Error>
    where
        Self: Sized;
}

/// Entity that gathers all keys provided by the KMS crate.
///
/// Key representations and public-key access are always available. Signing
/// delegation is derived only when all key variants support it (`prove`).
#[derive(Deserialize, ZeroizeOnDrop, PartialEq, Eq, Clone, Debug, KmsEnumKey)]
#[cfg_attr(feature = "prove", derive(KmsEnumSigningKey))]
#[cfg_attr(feature = "unsafe", derive(serde::Serialize))]
pub enum Key {
    Ed25519(Ed25519Key),
    Zk(ZkKey),
}

impl From<Ed25519Key> for Key {
    fn from(value: Ed25519Key) -> Self {
        Self::Ed25519(value)
    }
}

impl From<ZkKey> for Key {
    fn from(value: ZkKey) -> Self {
        Self::Zk(value)
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::*;

    #[test]
    fn ed25519_signing_is_available_without_zk_proving() {
        let key = Ed25519Key::from_bytes(&[7; 32]);
        let payload = Bytes::from_static(b"message");
        assert_eq!(
            SigningKey::sign(&key, &payload).unwrap(),
            key.sign_payload(&payload)
        );
    }

    #[test]
    fn mixed_key_enum_exposes_public_keys_without_signing() {
        let key = ZkKey::zero();
        let expected = key.to_public_key();
        assert_eq!(
            Key::Zk(key).as_public_key(),
            PublicKeyEncoding::Zk(expected)
        );
    }

    #[cfg(feature = "prove")]
    #[test]
    fn mixed_key_enum_signs_zk_payloads() {
        use lb_groth16::Fr;

        let key = ZkKey::new(Fr::from(1u64));
        let public_key = key.to_public_key();
        let payload = Fr::from(42u64);
        let signature = Key::Zk(key).sign(&PayloadEncoding::Zk(payload)).unwrap();
        let SignatureEncoding::Zk(signature) = signature else {
            panic!("ZK signing must retain the signature variant");
        };
        let inputs = public_inputs_from_pks(payload.into(), &[public_key]).unwrap();
        assert!(lb_zksign::verify(signature.as_proof(), &inputs).unwrap());
    }
}
