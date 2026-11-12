//! Key types, serialization, and public-key derivation.
//!
//! Signing is a separate capability provided by `SigningKey`. Ed25519 keys
//! always support it; ZK keys and the mixed `Key` enum require `prove`.

pub mod hd;
pub mod keys;

mod fixtures;

#[cfg(all(doctest, not(feature = "prove")))]
#[path = "../tests/compile_fail/signing.rs"]
mod signing_capabilities;
