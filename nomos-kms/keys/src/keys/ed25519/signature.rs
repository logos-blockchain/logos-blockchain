use core::hash::{Hash, Hasher};

use ed25519_dalek::SIGNATURE_LENGTH;
use serde::{Deserialize, Serialize};

pub const SIGNATURE_SIZE: usize = SIGNATURE_LENGTH;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Signature(ed25519_dalek::Signature);

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
