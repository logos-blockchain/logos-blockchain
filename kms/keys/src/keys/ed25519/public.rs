use core::fmt::{self, Debug, Formatter};

use ed25519_dalek::{PUBLIC_KEY_LENGTH, SignatureError, VerifyingKey};
use lb_binary_codec::bincode::BoundedSerializeOp;
use lb_utils::serde::{deserialize_bytes_array, serialize_bytes_array};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};

use crate::keys::{Ed25519Signature, UnverifiedX25519PublicKey, ed25519::x25519::X25519PublicKey};

pub const KEY_SIZE: usize = PUBLIC_KEY_LENGTH;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct UnverifiedPublicKey(VerifyingKey);

impl Serialize for UnverifiedPublicKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_bytes_array::<KEY_SIZE, _>(self.0.to_bytes(), serializer)
    }
}

impl Debug for UnverifiedPublicKey {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", hex::encode(self.0.as_bytes()))
    }
}

impl<'de> Deserialize<'de> for UnverifiedPublicKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes = deserialize_bytes_array::<KEY_SIZE, _>(deserializer)?;
        Ok(Self(VerifyingKey::from_bytes(&bytes).map_err(|_| {
            Error::custom("Invalid Ed25519 public key bytes.")
        })?))
    }
}

impl UnverifiedPublicKey {
    pub fn from_bytes(bytes: &[u8; KEY_SIZE]) -> Result<Self, SignatureError> {
        Ok(Self(VerifyingKey::from_bytes(bytes)?))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8; KEY_SIZE] {
        self.0.as_bytes()
    }

    #[must_use]
    pub fn to_bytes(&self) -> [u8; KEY_SIZE] {
        *self.as_bytes()
    }

    #[must_use]
    pub fn is_weak(&self) -> bool {
        self.0.is_weak()
    }

    pub fn verify(
        &self,
        message: &[u8],
        signature: &Ed25519Signature,
    ) -> Result<(), SignatureError> {
        self.0.verify_strict(message, signature.as_inner())
    }

    #[must_use]
    pub const fn into_inner(self) -> VerifyingKey {
        self.0
    }

    #[must_use]
    pub const fn as_inner(&self) -> &VerifyingKey {
        &self.0
    }

    #[must_use]
    pub fn derive_x25519(&self) -> UnverifiedX25519PublicKey {
        self.0.to_montgomery().to_bytes().into()
    }
}

impl BoundedSerializeOp for UnverifiedPublicKey {
    type Bytes = [u8; KEY_SIZE];
}

impl From<VerifyingKey> for UnverifiedPublicKey {
    fn from(value: VerifyingKey) -> Self {
        Self(value)
    }
}

impl From<UnverifiedPublicKey> for VerifyingKey {
    fn from(value: UnverifiedPublicKey) -> Self {
        value.0
    }
}

impl AsRef<[u8]> for UnverifiedPublicKey {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "UnverifiedPublicKey")]
pub struct PublicKey(UnverifiedPublicKey);

#[derive(thiserror::Error, Debug)]
pub enum KeyError {
    #[error(transparent)]
    Signature(#[from] SignatureError),
    #[error("Small order Ed255109 public key.")]
    SmallOrder,
}

impl TryFrom<UnverifiedPublicKey> for PublicKey {
    type Error = KeyError;

    fn try_from(value: UnverifiedPublicKey) -> Result<Self, Self::Error> {
        if value.0.is_weak() {
            return Err(KeyError::SmallOrder);
        }

        Ok(Self(value))
    }
}

impl TryFrom<VerifyingKey> for PublicKey {
    type Error = KeyError;

    fn try_from(value: VerifyingKey) -> Result<Self, Self::Error> {
        UnverifiedPublicKey::from(value).try_into()
    }
}

impl PublicKey {
    pub(super) fn from_verifying_key_unchecked(key: VerifyingKey) -> Self {
        Self(UnverifiedPublicKey::from(key))
    }

    pub fn from_bytes(bytes: &[u8; KEY_SIZE]) -> Result<Self, KeyError> {
        Self::try_from(UnverifiedPublicKey::from_bytes(bytes)?)
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8; KEY_SIZE] {
        self.0.as_bytes()
    }

    #[must_use]
    pub fn to_bytes(&self) -> [u8; KEY_SIZE] {
        *self.0.as_bytes()
    }

    pub fn verify(
        &self,
        message: &[u8],
        signature: &Ed25519Signature,
    ) -> Result<(), SignatureError> {
        self.0.verify(message, signature)
    }

    #[must_use]
    pub const fn as_unverified(&self) -> &UnverifiedPublicKey {
        &self.0
    }

    #[must_use]
    pub const fn into_unverified(self) -> UnverifiedPublicKey {
        self.0
    }

    #[must_use]
    pub const fn into_inner(self) -> VerifyingKey {
        self.0.into_inner()
    }

    #[must_use]
    pub const fn as_inner(&self) -> &VerifyingKey {
        self.0.as_inner()
    }

    #[must_use]
    pub fn derive_x25519(&self) -> X25519PublicKey {
        // A X25519 public key derived from a non-weak Ed25519 public key is never weak,
        // i.e., it always derives a contributory shared secret with any other key
        // derived the same way.
        X25519PublicKey::from_x25519_public_key_unchecked(self.0.derive_x25519())
    }
}

impl BoundedSerializeOp for PublicKey {
    type Bytes = [u8; KEY_SIZE];
}

impl From<PublicKey> for VerifyingKey {
    fn from(value: PublicKey) -> Self {
        value.0.into()
    }
}

impl AsRef<[u8]> for PublicKey {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use lb_binary_codec::bincode::{BoundedSerializeOp as _, SerializeOp};

    use super::UnverifiedPublicKey;

    #[test]
    fn public_key_has_exact_bincode_size() {
        let key = UnverifiedPublicKey::from_bytes(&[0x11; 32]).unwrap();
        let ordinary = <UnverifiedPublicKey as SerializeOp>::to_bytes(&key).unwrap();
        let bounded = key.to_bounded_bytes().unwrap();

        assert_eq!(ordinary.len(), 32);
        assert_eq!(bounded.as_ref(), ordinary.as_ref());
    }
}
