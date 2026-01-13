pub mod errors;
pub mod secured_key;

mod ed25519;
mod zk;

use key_management_system_macros::KmsEnumKey;
use serde::Deserialize;
use zeroize::ZeroizeOnDrop;

pub use crate::keys::{
    ed25519::{
        ED25519_PUBLIC_KEY_SIZE, ED25519_SECRET_KEY_SIZE, ED25519_SIGNATURE_SIZE, Ed25519Key,
        PublicKey as Ed25519PublicKey, Signature as Ed25519Signature, UnsecuredEd25519Key,
    },
    zk::{PublicKey as ZkPublicKey, Signature as ZkSignature, UnsecuredZkKey, ZkKey},
};

/// Entity that gathers all keys provided by the KMS crate.
///
/// Works as a [`SecuredKey`] over [`Encoding`], delegating requests to the
/// appropriate key.
#[derive(Deserialize, ZeroizeOnDrop, PartialEq, Eq, Clone, Debug, KmsEnumKey)]
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
