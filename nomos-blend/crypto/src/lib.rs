use blake2::{Blake2b512, digest::Digest as _};
use nomos_utils::blake_rng::{BlakeRng, RngCore as _, SeedableRng as _};

pub mod cipher;
pub mod keys;
pub mod merkle;

pub type ZkHash = groth16::Fr;
pub type ZkHasher = poseidon2::Poseidon2Bn254Hasher;

/// Generates random bytes of the constant size using [`BlakeRng`].
#[must_use]
pub fn random_sized_bytes<const SIZE: usize>() -> [u8; SIZE] {
    let mut buf = [0u8; SIZE];
    BlakeRng::from_entropy().fill_bytes(&mut buf);
    buf
}

/// Generates pseudo-random bytes of the constant size
/// using [`BlakeRng`] which is seeded with a hash of the domain and key.
#[must_use]
pub fn pseudo_random_sized_bytes<const SIZE: usize>(domain: &[u8], key: &[u8]) -> [u8; SIZE] {
    let mut buf = [0u8; SIZE];
    blake_random_bytes(&mut buf, domain, key);
    buf
}

/// Generates pseudo-random bytes of the given size
/// using [`BlakeRng`] which is seeded with a hash of the domain and key.
#[must_use]
pub fn pseudo_random_bytes(domain: &[u8], key: &[u8], size: usize) -> Vec<u8> {
    let mut buf = vec![0u8; size];
    blake_random_bytes(&mut buf, domain, key);
    buf
}

/// Writes pseudo-random bytes to the given buffer,
/// using [`BlakeRng`] which is seeded with a hash of the domain and key.
fn blake_random_bytes(buf: &mut [u8], domain: &[u8], key: &[u8]) {
    let mut cipher = BlakeRng::from_seed(blake2b512(&[domain, key]).into());
    cipher.fill_bytes(buf);
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
