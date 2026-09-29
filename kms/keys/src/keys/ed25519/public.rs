use core::fmt::{self, Debug, Formatter};

use ed25519_dalek::{PUBLIC_KEY_LENGTH, SignatureError, VerifyingKey};
use lb_binary_codec::bincode::BoundedSerializeOp;
use lb_utils::serde::{deserialize_bytes_array, serialize_bytes_array};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};

use crate::keys::{Ed25519Signature, UnverifiedX25519PublicKey, ed25519::x25519::X25519PublicKey};

pub const KEY_SIZE: usize = PUBLIC_KEY_LENGTH;

/// An Ed25519 public key that has not been verified for small order.
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

    pub fn verify(
        &self,
        message: &[u8],
        signature: &Ed25519Signature,
    ) -> Result<(), SignatureError> {
        self.0.verify_strict(message, signature.as_inner())
    }

    /// Checks if the public key is weak (i.e., has small order).
    #[must_use]
    pub fn is_weak(&self) -> bool {
        self.0.is_weak()
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
    use curve25519_dalek::constants::EIGHT_TORSION;
    use ed25519_dalek::VerifyingKey;
    use lb_binary_codec::{
        bincode::{BoundedSerializeOp as _, SerializeOp},
        canonical::{BinaryDecode as _, BinaryEncode as _},
    };

    use super::{KEY_SIZE, KeyError, PublicKey, UnverifiedPublicKey};
    use crate::keys::{Ed25519Key, Ed25519Signature, X25519PrivateKey};

    /// The canonical encodings of the eight points whose order divides the
    /// cofactor. Taken from the curve constants rather than written out, so the
    /// vectors cannot silently drift from what the curve actually defines.
    fn small_order_keys() -> [[u8; KEY_SIZE]; 8] {
        EIGHT_TORSION.map(|point| point.compress().to_bytes())
    }

    /// Guards the vectors above: if these were not small order, every rejection
    /// test below would pass vacuously.
    #[test]
    fn the_torsion_vectors_really_are_small_order() {
        for bytes in small_order_keys() {
            let key = UnverifiedPublicKey::from_bytes(&bytes)
                .expect("a torsion point is a well-formed curve point");
            assert!(key.as_inner().is_weak(), "{}", hex::encode(bytes));
        }
    }

    #[test]
    fn small_order_keys_are_rejected_on_every_route_in() {
        for bytes in small_order_keys() {
            let unverified = UnverifiedPublicKey::from_bytes(&bytes).unwrap();
            let hex = hex::encode(bytes);

            assert!(
                matches!(PublicKey::from_bytes(&bytes), Err(KeyError::SmallOrder)),
                "from_bytes accepted {hex}"
            );
            assert!(
                matches!(PublicKey::try_from(unverified), Err(KeyError::SmallOrder)),
                "TryFrom<UnverifiedPublicKey> accepted {hex}"
            );
            assert!(
                matches!(
                    PublicKey::try_from(VerifyingKey::from_bytes(&bytes).unwrap()),
                    Err(KeyError::SmallOrder)
                ),
                "TryFrom<VerifyingKey> accepted {hex}"
            );

            // Deserialization, in both the human-readable and the binary form.
            let json = serde_json::to_string(&unverified).unwrap();
            assert!(
                serde_json::from_str::<PublicKey>(&json).is_err(),
                "JSON deserialization accepted {hex}"
            );
            let encoded = unverified.encode();
            assert!(
                PublicKey::decode(&encoded, &()).is_err(),
                "binary decoding accepted {hex}"
            );
        }
    }

    /// The unverified type is the escape hatch, so it has to keep accepting
    /// what the checked one turns away -- the genesis placeholder is one of
    /// these points.
    #[test]
    fn the_unverified_key_still_accepts_them() {
        for bytes in small_order_keys() {
            let key = UnverifiedPublicKey::from_bytes(&bytes).unwrap();
            assert_eq!(key.to_bytes(), bytes);

            let json = serde_json::to_string(&key).unwrap();
            assert_eq!(
                serde_json::from_str::<UnverifiedPublicKey>(&json).unwrap(),
                key
            );
            let encoded = key.encode();
            let (rest, decoded) = UnverifiedPublicKey::decode(&encoded, &()).unwrap();
            assert!(rest.is_empty());
            assert_eq!(decoded, key);
        }
    }

    /// Clamping forces every secret scalar to a multiple of the cofactor that
    /// stays below the group order, so a derived key always has full order.
    /// This is what lets `Ed25519Key::public_key` skip the check.
    #[test]
    fn keys_derived_from_a_secret_are_never_small_order() {
        for seed in 0..=u8::MAX {
            let derived = Ed25519Key::from_bytes(&[seed; 32]).public_key();
            assert!(!derived.as_inner().is_weak(), "seed {seed}");
            // ...and so the checked constructor agrees with the unchecked one.
            PublicKey::from_bytes(&derived.to_bytes())
                .unwrap_or_else(|error| panic!("seed {seed} was rejected: {error}"));
        }
    }

    /// `verify_strict` turns a small-order key down before it looks at the
    /// signature, which is why an accredited key of this shape leaves a channel
    /// unpostable instead of forgeable.
    #[test]
    fn a_small_order_key_verifies_nothing() {
        let signing_key = Ed25519Key::from_bytes(&[4; 32]);
        let message = b"logos";
        let genuine = signing_key.sign_payload(message);

        for bytes in small_order_keys() {
            let key = UnverifiedPublicKey::from_bytes(&bytes).unwrap();
            assert!(key.verify(message, &genuine).is_err());
            assert!(
                key.verify(message, &Ed25519Signature::from_bytes(&[0; _]))
                    .is_err()
            );
        }
    }

    /// The reason `derive_shared_key` can be infallible: a small-order
    /// key derives a Montgomery point that makes the exchange non-contributory,
    /// and a checked key cannot be one.
    #[test]
    fn only_a_checked_key_guarantees_a_contributory_exchange() {
        let secret = X25519PrivateKey::from([7u8; 32]);

        for bytes in small_order_keys() {
            let weak = UnverifiedPublicKey::from_bytes(&bytes).unwrap();
            assert!(
                secret
                    .try_derive_shared_key(&weak.derive_x25519())
                    .is_none(),
                "{} produced a contributory exchange",
                hex::encode(bytes)
            );
        }

        for seed in [0u8, 1, 42, 255] {
            let checked = Ed25519Key::from_bytes(&[seed; 32]).public_key();
            // Would panic if the exchange were non-contributory.
            drop(secret.derive_shared_key(&checked.derive_x25519()));
        }
    }

    #[test]
    fn public_key_has_exact_bincode_size() {
        let key = UnverifiedPublicKey::from_bytes(&[0x11; 32]).unwrap();
        let ordinary = <UnverifiedPublicKey as SerializeOp>::to_bytes(&key).unwrap();
        let bounded = key.to_bounded_bytes().unwrap();

        assert_eq!(ordinary.len(), 32);
        assert_eq!(bounded.as_ref(), ordinary.as_ref());
    }
}
