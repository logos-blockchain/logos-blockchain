//! ZK-signature proof types and verification.
//!
//! Enable `prove` to generate signatures using the native witness generator and
//! prover.

mod public;
mod verification_key;
mod verify;

#[cfg(feature = "prove")]
mod proving;

#[cfg(feature = "prove")]
pub use proving::{ProveError, ZkSignPrivateKeysData, ZkSignWitnessInputs, prove};
pub use public::ZkSignVerifierInputs;
pub use verify::{VerifyError, batch_verify, verify};

pub type ZkSignProof = lb_groth16::CompressedGroth16Proof;

#[derive(Debug, PartialEq, Eq, thiserror::Error, Clone)]
pub enum ZkSignError {
    #[error("ZkSign supports up to 32 keys: got {0}")]
    TooManyKeys(usize),
}
