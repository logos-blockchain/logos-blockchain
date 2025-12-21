use core::fmt::{self, Debug, Formatter};

use bytes::Bytes;
use ed25519_dalek::SigningKey;
use rand_core::CryptoRngCore;
use serde::Deserialize;
use zeroize::ZeroizeOnDrop;

use crate::keys::{errors::KeyError, secured_key::SecuredKey};

mod private;
pub use self::private::{KEY_SIZE as ED25519_SECRET_KEY_SIZE, UnsecuredEd25519Key};
mod public;
pub use self::public::{KEY_SIZE as ED25519_PUBLIC_KEY_SIZE, PublicKey};
mod signature;
pub use self::signature::{SIGNATURE_SIZE as ED25519_SIGNATURE_SIZE, Signature};

/// An hardened Ed25519 secret key that only exposes methods to retrieve public
/// information.
///
/// It is a secured variant of a [`UnsecuredEd25519Key`] and used within the set
/// of supported KMS keys.
#[derive(Deserialize, ZeroizeOnDrop, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "unsafe", derive(serde::Serialize))]
pub struct Ed25519Key(UnsecuredEd25519Key);

impl Ed25519Key {
    #[must_use]
    pub const fn new(signing_key: SigningKey) -> Self {
        Self(UnsecuredEd25519Key(signing_key))
    }

    pub fn generate<Rng>(rng: &mut Rng) -> Self
    where
        Rng: CryptoRngCore,
    {
        Self(UnsecuredEd25519Key::generate(rng))
    }

    #[must_use]
    pub fn from_bytes(bytes: &[u8; ED25519_SECRET_KEY_SIZE]) -> Self {
        Self(UnsecuredEd25519Key::from_bytes(bytes))
    }

    #[must_use]
    pub fn public_key(&self) -> PublicKey {
        self.0.public_key()
    }

    #[must_use]
    pub fn sign_payload(&self, payload: &[u8]) -> Signature {
        self.0.sign_payload(payload)
    }
}

impl From<SigningKey> for Ed25519Key {
    fn from(value: SigningKey) -> Self {
        Self(UnsecuredEd25519Key::from(value))
    }
}

impl Debug for Ed25519Key {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        #[cfg(feature = "unsafe")]
        write!(f, "Ed25519Key({:?})", self.0)?;

        #[cfg(not(feature = "unsafe"))]
        write!(f, "Ed25519Key(<redacted>)")?;

        Ok(())
    }
}

#[cfg(feature = "unsafe")]
impl From<UnsecuredEd25519Key> for Ed25519Key {
    fn from(value: UnsecuredEd25519Key) -> Self {
        Self(value)
    }
}

#[async_trait::async_trait]
impl SecuredKey for Ed25519Key {
    type Payload = Bytes;
    type Signature = Signature;
    type PublicKey = PublicKey;
    type Error = KeyError;

    fn sign(&self, payload: &Self::Payload) -> Result<Self::Signature, Self::Error> {
        Ok(self.sign_payload(payload.iter().as_slice()))
    }

    fn sign_multiple(
        _keys: &[&Self],
        _payload: &Self::Payload,
    ) -> Result<Self::Signature, Self::Error> {
        unimplemented!("Multi-key signature is not implemented for Ed25519 keys.")
    }

    fn as_public_key(&self) -> Self::PublicKey {
        self.public_key()
    }
}
