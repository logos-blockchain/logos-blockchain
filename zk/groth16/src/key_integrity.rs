//! Integrity pinning for the Groth16 key artefacts embedded in the binary.
//!
//! The proving and verification keys are not part of this repository: they are
//! fetched at build time and embedded as raw bytes. Pinning their digest here
//! is what ties the bytes a node ends up using to the audited trusted setup
//! output.

use sha2::{Digest as _, Sha256};

/// Hex-encoded SHA-256 digest of a key artefact.
#[must_use]
pub fn key_sha256(key: &[u8]) -> String {
    hex::encode(Sha256::digest(key))
}

/// Returns `key` after checking its SHA-256 digest against the one pinned in
/// this repository.
///
/// # Panics
///
/// If the digest of `key` is not `pinned_sha256`: a key other than the pinned
/// one must never be used to prove or verify.
#[must_use]
pub fn pinned_key<'key>(name: &str, key: &'key [u8], pinned_sha256: &str) -> &'key [u8] {
    assert_eq!(
        key_sha256(key),
        pinned_sha256,
        "{name} does not match its pinned SHA-256 digest"
    );
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn pinned_key_accepts_matching_digest() {
        assert_eq!(pinned_key("test key", b"abc", ABC_SHA256), b"abc");
    }

    #[test]
    #[should_panic(expected = "test key does not match its pinned SHA-256 digest")]
    fn pinned_key_rejects_other_bytes() {
        let _ = pinned_key("test key", b"abd", ABC_SHA256);
    }
}
