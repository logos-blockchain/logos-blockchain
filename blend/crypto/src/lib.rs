use blake2::{Blake2b512, digest::Digest as _};
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
    let mut rng = chacha20_rng(&blake2b512(&[domain, key]));
    rng.fill_bytes(buf);
}

/// Builds the CSPRBG defined by the ChaCha20-Based PRNG Construction of the
/// spec: a [`ChaCha20Rng`] keyed with the first 32 bytes of the seed.
#[must_use]
pub fn chacha20_rng(seed: &[u8; 64]) -> ChaCha20Rng {
    let mut key = [0u8; 32];
    key.copy_from_slice(&seed[..32]);
    ChaCha20Rng::from_seed(key)
}

/// Computes the BLAKE2b-512 hash of the concatenated inputs.
#[must_use]
pub fn blake2b512(inputs: &[&[u8]]) -> [u8; 64] {
    let mut hasher = Blake2b512::new();
    for input in inputs {
        hasher.update(input);
    }
    hasher.finalize().into()
}
