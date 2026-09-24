use subtle::ConstantTimeEq as _;
use x25519_dalek::{SharedSecret, StaticSecret};
use zeroize::ZeroizeOnDrop;

pub const X25519_SECRET_KEY_LENGTH: usize = 32;

#[derive(Clone, ZeroizeOnDrop)]
pub struct X25519PrivateKey(StaticSecret);

impl X25519PrivateKey {
    /// Performs a Diffie-Hellman key exchange with an unverified X25519 public
    /// key.
    ///
    /// Returns `Some(SharedKey)` if the key exchange was contributory,
    /// otherwise `None`.
    #[must_use]
    pub fn try_derive_shared_key(
        &self,
        public_key: &UnverifiedX25519PublicKey,
    ) -> Option<SharedKey> {
        let shared_key = self.0.diffie_hellman(&public_key.0);
        shared_key.was_contributory().then(|| SharedKey(shared_key))
    }

    /// Performs a Diffie-Hellman key exchange with a verified X25519 public
    /// key.
    ///
    /// Returns the derived shared key.
    #[must_use]
    pub fn derive_shared_key(&self, public_key: &X25519PublicKey) -> SharedKey {
        self.try_derive_shared_key(&public_key.0).expect("Shared key derivation failed: non-contributory key exchange. This should not happen with hardened public keys.")
    }
}

impl From<[u8; X25519_SECRET_KEY_LENGTH]> for X25519PrivateKey {
    fn from(bytes: [u8; X25519_SECRET_KEY_LENGTH]) -> Self {
        Self(StaticSecret::from(bytes))
    }
}

impl PartialEq for X25519PrivateKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_bytes().ct_eq(other.0.as_bytes()).into()
    }
}

impl Eq for X25519PrivateKey {}

pub const X25519_PUBLIC_KEY_LENGTH: usize = 32;

#[derive(Clone, Copy)]
pub struct UnverifiedX25519PublicKey(x25519_dalek::PublicKey);

impl From<[u8; X25519_PUBLIC_KEY_LENGTH]> for UnverifiedX25519PublicKey {
    fn from(bytes: [u8; X25519_PUBLIC_KEY_LENGTH]) -> Self {
        Self(x25519_dalek::PublicKey::from(bytes))
    }
}

impl From<UnverifiedX25519PublicKey> for [u8; X25519_PUBLIC_KEY_LENGTH] {
    fn from(key: UnverifiedX25519PublicKey) -> Self {
        key.0.to_bytes()
    }
}

#[derive(Clone, Copy)]
pub struct X25519PublicKey(UnverifiedX25519PublicKey);

impl X25519PublicKey {
    pub(super) const fn from_x25519_public_key_unchecked(
        public_key: UnverifiedX25519PublicKey,
    ) -> Self {
        Self(public_key)
    }
}

#[derive(ZeroizeOnDrop)]
pub struct SharedKey(SharedSecret);

impl SharedKey {
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.0.as_bytes()[..]
    }
}

impl PartialEq for SharedKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_bytes().ct_eq(other.0.as_bytes()).into()
    }
}

impl Eq for SharedKey {}
