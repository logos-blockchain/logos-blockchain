pub mod inputs;
pub mod private;
pub mod public;

use groth16::CompressedGroth16Proof;

pub use inputs::ZkSignWitnessInputs;
pub use private::ZkSignPrivateKeysData;
pub use public::ZkSignVerifierInputs;

#[derive(Debug, PartialEq, Eq, thiserror::Error, Clone)]
pub enum ZkSignError {
    #[error("ZkSign supports up to 32 keys: got {0}")]
    TooManyKeys(usize),
}

pub type ZkSignProof = CompressedGroth16Proof;
