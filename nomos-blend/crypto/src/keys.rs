use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq as _;
use x25519_dalek::StaticSecret;
use zeroize::ZeroizeOnDrop;

use crate::cipher::Cipher;

pub const X25519_SECRET_KEY_LENGTH: usize = 32;

#[derive(Clone, ZeroizeOnDrop)]
pub struct X25519PrivateKey(StaticSecret);

impl X25519PrivateKey {
    #[must_use]
    pub fn derive_shared_key(&self, public_key: &X25519PublicKey) -> SharedKey {
        SharedKey(self.0.diffie_hellman(&public_key.0).to_bytes())
    }
}

impl From<[u8; X25519_SECRET_KEY_LENGTH]> for X25519PrivateKey {
    fn from(bytes: [u8; X25519_SECRET_KEY_LENGTH]) -> Self {
        Self(StaticSecret::from(bytes))
    }
}

impl From<X25519PrivateKey> for [u8; X25519_SECRET_KEY_LENGTH] {
    fn from(key: X25519PrivateKey) -> Self {
        key.0.to_bytes()
    }
}

impl PartialEq for X25519PrivateKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_bytes().ct_eq(other.0.as_bytes()).into()
    }
}

impl Eq for X25519PrivateKey {}

pub const X25519_PUBLIC_KEY_LENGTH: usize = 32;

#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct X25519PublicKey(x25519_dalek::PublicKey);

impl From<[u8; X25519_PUBLIC_KEY_LENGTH]> for X25519PublicKey {
    fn from(bytes: [u8; X25519_PUBLIC_KEY_LENGTH]) -> Self {
        Self(x25519_dalek::PublicKey::from(bytes))
    }
}

impl From<X25519PublicKey> for [u8; X25519_PUBLIC_KEY_LENGTH] {
    fn from(key: X25519PublicKey) -> Self {
        key.0.to_bytes()
    }
}

pub const X25519_SHARED_KEY_LENGTH: usize = 32;

#[derive(Clone, ZeroizeOnDrop)]
pub struct SharedKey([u8; X25519_SHARED_KEY_LENGTH]);

impl SharedKey {
    #[must_use]
    pub const fn as_slice(&self) -> &[u8] {
        &self.0
    }

    #[must_use]
    pub fn cipher(&self, domain: &[u8]) -> Cipher {
        Cipher::new(domain, &self.0)
    }
}

impl PartialEq for SharedKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.ct_eq(&other.0).into()
    }
}

impl Eq for SharedKey {}
