use core::hash::{Hash, Hasher};

use ed25519_dalek::SIGNATURE_LENGTH;
use lb_utils::serde::{deserialize_bytes_array, serialize_bytes_array};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const SIGNATURE_SIZE: usize = SIGNATURE_LENGTH;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signature(ed25519_dalek::Signature);

impl Serialize for Signature {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serialize_bytes_array::<SIGNATURE_SIZE, _>(self.0.to_bytes(), serializer)
    }
}

impl<'de> Deserialize<'de> for Signature {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes = deserialize_bytes_array::<SIGNATURE_SIZE, _>(deserializer)?;
        Ok(Self(ed25519_dalek::Signature::from_bytes(&bytes)))
    }
}

impl Signature {
    #[must_use]
    pub fn from_bytes(bytes: &[u8; SIGNATURE_SIZE]) -> Self {
        Self(ed25519_dalek::Signature::from_bytes(bytes))
    }

    #[must_use]
    pub fn to_bytes(&self) -> [u8; SIGNATURE_SIZE] {
        self.0.to_bytes()
    }

    #[must_use]
    pub const fn as_inner(&self) -> &ed25519_dalek::Signature {
        &self.0
    }

    #[must_use]
    pub fn zero() -> Self {
        Self::from_bytes(&[0u8; 64])
    }
}

impl Hash for Signature {
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        self.0.to_bytes().hash(state);
    }
}

impl From<ed25519_dalek::Signature> for Signature {
    fn from(sig: ed25519_dalek::Signature) -> Self {
        Self(sig)
    }
}

impl From<Signature> for ed25519_dalek::Signature {
    fn from(sig: Signature) -> Self {
        sig.0
    }
}

impl From<[u8; SIGNATURE_SIZE]> for Signature {
    fn from(bytes: [u8; SIGNATURE_SIZE]) -> Self {
        Self::from_bytes(&bytes)
    }
}
