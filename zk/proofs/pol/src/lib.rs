//! # zk-pol
//!
//! ## Usage
//!
//! The library provides a single function, `prove`, which takes a set of
//! inputs and generates a proof. The function returns a tuple containing the
//! generated proof and the corresponding public inputs.
//! A normal flow of usage will involve the following steps:
//! 1. Fill out some `PolChainData` with the public inputs
//! 2. Fill out some `PolWalletData` with the private inputs
//! 3. Construct the `PolWitnessInputs` from the `PolChainData` and
//!    `PolWalletData`
//! 4. Call `prove` with the `PolWitnessInputs`
//! 5. Use the returned proof and public inputs to verify the proof
//!
//! ## Example
//!
//! ```ignore
//! use zk_pol::{prove, PolChainInputs, PolChainInputsData, PolWalletInputs, PolWalletInputsData};
//!
//! fn main() {
//!     let chain_data = PolChainInputsData {..};
//!     let wallet_data = PolWalletInputsData {..};
//!     let witness_inputs = PolWitnessInputs::from_chain_and_wallet_data(chain_data, wallet_data).unwrap();
//!     let (proof, inputs) = prove(&witness_inputs).unwrap();
//!     assert!(verify(&proof, &inputs).unwrap());
//! }

mod chain_inputs;
mod inputs;
mod lottery;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;
mod verification_key;
mod wallet_inputs;
mod witness;

use std::error::Error;

pub use chain_inputs::{PolChainInputs, PolChainInputsData};
pub use inputs::{PolVerifierInput, PolWitnessInputs, PolWitnessInputsData};
use lb_circuits_prover::Prover as _;
use lb_groth16::{CompressedGroth16Proof, Groth16Proof, Groth16ProofJsonDeser};
use lb_log_targets::proofs;
use tracing::error;
pub use wallet_inputs::{NOTE_SET_TREE_DEPTH, PolWalletInputs, PolWalletInputsData};

use crate::inputs::PolVerifierInputJson;
pub use crate::lottery::{LotteryConstants, P};

pub type PoLProof = CompressedGroth16Proof;
pub type ProveError = lbp_error::Error;

const LOG_TARGET: &str = proofs::POL;

///
/// This function generates a proof for the given set of inputs.
///
/// # Arguments
/// - `inputs`: A reference to `PolWitnessInputs`, which contains the necessary
///   data to generate the witness and construct the proof.
///
/// # Returns
/// - `Ok((PoLProof, PolVerifierInput))`: On success, returns a tuple containing
///   the generated proof (`PoLProof`) and the corresponding public inputs
///   (`PolVerifierInput`).
/// - `Err(ProveError)`: On failure, returns an error of type `ProveError`,
///   which can occur due to I/O errors or JSON (de)serialization errors.
///
/// # Errors
/// - Returns a `ProveError::Io` if an I/O error occurs while generating the
///   witness or proving from contents.
/// - Returns a `ProveError::Json` if there is an error during JSON
///   serialization or deserialization.
pub fn prove(inputs: PolWitnessInputs) -> Result<(PoLProof, PolVerifierInput), ProveError> {
    let witness = witness::generate_witness(inputs)?;
    let result = lb_circuits_prover::Rapidsnark::prove(
        lbc_pol_sys::artifacts::PROVING_KEY,
        witness.as_ref(),
    )?;
    let proof: Groth16ProofJsonDeser = serde_json::from_str(&result.proof)?;
    let verifier_inputs: PolVerifierInputJson = serde_json::from_str(&result.public_signals)?;
    let proof: Groth16Proof = proof.try_into().map_err(ProveError::Groth16JsonProof)?;
    Ok((
        CompressedGroth16Proof::try_from(&proof).unwrap_or_else(|e| {
            error!(target: LOG_TARGET, "Fatal CompressedGroth16Proof::try_from: {e}");
            // We panic here because this should never happen, and if it does, it's a
            // critical error that we want to be immediately visible during
            // development and testing.
            panic!("Fatal CompressedGroth16Proof::try_from: {e}")
        }),
        verifier_inputs
            .try_into()
            .map_err(ProveError::Groth16JsonInput)?,
    ))
}

#[derive(Debug)]
pub enum VerifyError {
    Expansion,
    ProofVerify(Box<dyn Error>),
}

///
/// This function verifies a proof against a set of public inputs.
///
/// # Arguments
///
/// - `proof`: A reference to the proof (`PoLProof`) that needs verification.
/// - `public_inputs`: A reference to `PolVerifierInput`, which contains the
///   public inputs against which the proof is verified.
///
/// # Returns
///
/// - `Ok(true)`: If the proof is successfully verified against the public
///   inputs.
/// - `Ok(false)`: If the proof is invalid when compared with the public inputs.
/// - `Err`: If an error occurs during the verification process.
///
/// # Errors
///
/// - Returns an error if there is an issue with the verification key or the
///   underlying verification process fails.
pub fn verify(proof: &PoLProof, public_inputs: &PolVerifierInput) -> Result<bool, VerifyError> {
    let inputs = public_inputs.to_inputs();
    let expanded_proof = Groth16Proof::try_from(proof).map_err(|_| VerifyError::Expansion)?;
    lb_groth16::groth16_verify(verification_key::POL_VK.as_ref(), &expanded_proof, &inputs)
        .map_err(|e| VerifyError::ProofVerify(Box::new(e)))
}

#[cfg(test)]
mod tests {
    use lb_groth16::{Field as _, Fr, Groth16Input};

    use super::*;

    #[test]
    fn test_full_flow() {
        let data = test_utils::sample_witness_data();
        let msg = data.chain.msg;
        let (proof, inputs) = prove(data.into()).unwrap();
        assert_eq!(inputs.slot_number.into_inner(), Fr::from(678u64));
        assert_eq!(inputs.msg.into_inner(), msg);
        assert!(verify(&proof, &inputs).unwrap());
        let rebound = PolVerifierInput {
            msg: Groth16Input::new(msg + Fr::ONE),
            ..inputs
        };
        assert!(!verify(&proof, &rebound).unwrap());
    }
}
