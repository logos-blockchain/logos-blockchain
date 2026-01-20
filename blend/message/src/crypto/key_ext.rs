use lb_blend_crypto::cipher::Cipher;
use lb_key_management_system_keys::{
    keys::UnsecuredEd25519Key, operators::ed25519::derive_x25519::SharedKey,
};
use lb_utils::blake_rng::{BlakeRng, SeedableRng as _};
use zeroize::ZeroizeOnDrop;

// This extension trait must go here instead of `logos-blockchain-blend-crypto`
// because else we would have a circular dependency between that and
// `key-management-system-keys`. Also, these extension functions are mostly used
// in this crate, so it makes most sense for them to be defined here.
pub trait Ed25519SecretKeyExt: ZeroizeOnDrop {
    fn generate_with_blake_rng() -> Self;
}

impl Ed25519SecretKeyExt for UnsecuredEd25519Key {
    fn generate_with_blake_rng() -> Self {
        Self::generate(&mut BlakeRng::from_entropy())
    }
}

pub(crate) trait SharedKeyExt {
    fn cipher(&self, domain: &[u8]) -> Cipher;
}

impl SharedKeyExt for SharedKey {
    fn cipher(&self, domain: &[u8]) -> Cipher {
        Cipher::new(domain, self.as_slice())
    }
}
