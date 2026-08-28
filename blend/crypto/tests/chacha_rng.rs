//! Tests for the ChaCha20-based PRNG, as specified by the ChaCha20-Based PRNG
//! Construction in the Common Cryptographic Components specification.

use logos_blockchain_blend_crypto::chacha20_rng;
use rand::RngCore as _;

/// The spec pins the PRNG to the `ChaCha20` keystream; this guards against a
/// dependency bump silently changing the stream.
#[test]
fn test_keystream_stability() {
    let mut rng = chacha20_rng(&[0u8; 64]);
    let mut out = [0u8; 8];
    rng.fill_bytes(&mut out);
    // First 8 keystream bytes of ChaCha20 under an all-zero key and nonce,
    // per the reference implementation.
    assert_eq!(out, [0x76, 0xb8, 0xe0, 0xad, 0xa0, 0xf1, 0x3d, 0x90]);
}

/// Only the first 32 bytes of the 64-byte seed key the stream.
#[test]
fn test_seed_truncation() {
    let mut seed = [0u8; 64];
    let mut base = [0u8; 8];
    chacha20_rng(&seed).fill_bytes(&mut base);

    seed[32] = 0xFF;
    let mut upper_half_changed = [0u8; 8];
    chacha20_rng(&seed).fill_bytes(&mut upper_half_changed);
    assert_eq!(base, upper_half_changed);

    seed[0] = 0xFF;
    let mut key_changed = [0u8; 8];
    chacha20_rng(&seed).fill_bytes(&mut key_changed);
    assert_ne!(base, key_changed);
}
