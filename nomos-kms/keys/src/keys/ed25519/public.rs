use ed25519_dalek::{PUBLIC_KEY_LENGTH, SignatureError, Verifier as _, VerifyingKey};
use nomos_utils::serde::{deserialize_bytes_array, serialize_bytes_array};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};

use crate::keys::Ed25519Signature;

pub const KEY_SIZE: usize = PUBLIC_KEY_LENGTH;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublicKey(VerifyingKey);

impl Serialize for PublicKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_bytes_array::<KEY_SIZE, _>(self.0.to_bytes(), serializer)
    }
}

impl<'de> Deserialize<'de> for PublicKey {
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

impl PublicKey {
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

    pub fn verify(
        &self,
        message: &[u8],
        signature: &Ed25519Signature,
    ) -> Result<(), SignatureError> {
        self.0.verify(message, signature.as_inner())
    }

    #[must_use]
    pub const fn into_inner(self) -> VerifyingKey {
        self.0
    }

    #[must_use]
    pub const fn as_inner(&self) -> &VerifyingKey {
        &self.0
    }
}

impl From<VerifyingKey> for PublicKey {
    fn from(value: VerifyingKey) -> Self {
        Self(value)
    }
}

impl From<PublicKey> for VerifyingKey {
    fn from(value: PublicKey) -> Self {
        value.0
    }
}

impl AsRef<[u8]> for PublicKey {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}
