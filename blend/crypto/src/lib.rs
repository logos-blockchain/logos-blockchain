use blake2::{
    Blake2b,
    digest::{Digest as _, consts::U32},
};
use rand::{RngCore as _, SeedableRng as _};
use rand_chacha::ChaCha20Rng;

pub mod cipher;
pub mod merkle;

pub type ZkHash = lb_groth16::Fr;
pub type ZkHasher = lb_poseidon2::Poseidon2Bn254Hasher;

/// Generates random bytes of the constant size using [`ChaCha20Rng`].
#[must_use]
pub fn random_sized_bytes<const SIZE: usize>() -> [u8; SIZE] {
    let mut buf = [0u8; SIZE];
    fill_random_bytes(&mut buf);
    buf
}

pub fn fill_random_bytes(buf: &mut [u8]) {
    ChaCha20Rng::from_entropy().fill_bytes(buf);
}

/// Generates pseudo-random bytes of the constant size
/// using [`ChaCha20Rng`] which is seeded with a hash of the domain and key.
#[must_use]
pub fn pseudo_random_sized_bytes<const SIZE: usize>(domain: &[u8], key: &[u8]) -> [u8; SIZE] {
    let mut buf = [0u8; SIZE];
    pseudo_random_bytes(&mut buf, domain, key);
    buf
}

/// Writes pseudo-random bytes to the given buffer,
/// using [`ChaCha20Rng`] which is seeded with a hash of the domain and key.
fn pseudo_random_bytes(buf: &mut [u8], domain: &[u8], key: &[u8]) {
    let mut rng = ChaCha20Rng::from_seed(chacha20_seed(domain, key));
    rng.fill_bytes(buf);
}

/// Derives the 32-byte CSPRBG seed for a domain and key, per the spec's
/// ChaCha20-Based PRNG Construction: the domain-separated BLAKE2b-256 digest
/// is the seed.
pub(crate) fn chacha20_seed(domain: &[u8], key: &[u8]) -> [u8; 32] {
    let mut hasher = Blake2b::<U32>::new();
    hasher.update(domain);
    hasher.update(key);
    hasher.finalize().into()
}
