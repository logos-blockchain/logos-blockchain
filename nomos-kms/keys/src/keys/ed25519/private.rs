use ed25519_dalek::{SECRET_KEY_LENGTH, SigningKey, ed25519::signature::Signer as _};
use nomos_utils::serde::{deserialize_bytes_array, serialize_bytes_array};
use rand_core::CryptoRngCore;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use subtle::ConstantTimeEq as _;
use zeroize::ZeroizeOnDrop;

use crate::keys::{Ed25519PublicKey, Ed25519Signature};

pub const KEY_SIZE: usize = SECRET_KEY_LENGTH;

/// An Ed25519 secret key exposing methods to retrieve its inner secret value.
///
/// To be used in contexts where a KMS-like key is required, but it's not
/// possible to go through the KMS roundtrip of executing operators.
#[derive(ZeroizeOnDrop, Clone, Debug)]
pub struct UnsecuredEd25519Key(pub(super) SigningKey);

impl UnsecuredEd25519Key {
    #[must_use]
    pub fn from_bytes(bytes: &[u8; SECRET_KEY_LENGTH]) -> Self {
        Self(SigningKey::from_bytes(bytes))
    }

    #[must_use]
    pub fn public_key(&self) -> Ed25519PublicKey {
        self.0.verifying_key().into()
    }

    pub fn generate<Rng>(rng: &mut Rng) -> Self
    where
        Rng: CryptoRngCore,
    {
        Self(SigningKey::generate(rng))
    }

    #[must_use]
    pub fn sign_payload(&self, payload: &[u8]) -> Ed25519Signature {
        self.0.sign(payload).into()
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8; SECRET_KEY_LENGTH] {
        self.0.as_bytes()
    }

    /// Return a reference to the inner secret key.
    #[must_use]
    pub const fn as_inner(&self) -> &SigningKey {
        &self.0
    }
}

impl Serialize for UnsecuredEd25519Key {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_bytes_array::<KEY_SIZE, _>(self.0.to_bytes(), serializer)
    }
}

impl<'de> Deserialize<'de> for UnsecuredEd25519Key {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes = deserialize_bytes_array::<KEY_SIZE, _>(deserializer)?;
        Ok(Self(SigningKey::from_bytes(&bytes)))
    }
}

impl PartialEq for UnsecuredEd25519Key {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_bytes().ct_eq(other.0.as_bytes()).into()
    }
}

impl Eq for UnsecuredEd25519Key {}

impl From<SigningKey> for UnsecuredEd25519Key {
    fn from(value: SigningKey) -> Self {
        Self(value)
    }
}

impl From<UnsecuredEd25519Key> for SigningKey {
    fn from(value: UnsecuredEd25519Key) -> Self {
        value.0.clone()
    }
}
