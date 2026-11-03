//! # zk-zktransfer
//!
//! ## Usage
//!
//! The library provides a single function, `prove`, which takes a set of
//! inputs and generates a proof. The function returns a tuple containing the
//! generated proof and the corresponding public inputs.
//! A normal flow of usage will involve the following steps:
//! 1. Fill out some `ZkTransferChainInputsData` with the public inputs
//! 2. Fill out some `ZkTransferWalletInputsData` with the private inputs
//! 3. Construct the `ZkTransferWitnessInputs` from the
//!    `ZkTransferChainInputsData` and `ZkTransferWalletInputsData`
//! 4. Call `prove` with the `ZkTransferWitnessInputs`
//! 5. Use the returned proof and public inputs to verify the proof
//!
//! ## Example
//!
//! ```ignore
//! use zk_zktransfer::{prove, verify, ZkTransferChainInputsData, ZkTransferWalletInputsData, ZkTransferWitnessInputs};
//!
//! fn main() {
//!     let chain_data = ZkTransferChainInputsData {..};
//!     let wallet_data = ZkTransferWalletInputsData {..};
//!     let witness_inputs = ZkTransferWitnessInputs::from_chain_and_wallet_data(chain_data, wallet_data);
//!     let (proof, inputs) = prove(witness_inputs).unwrap();
//!     assert!(verify(&proof, &inputs).unwrap());
//! }

mod chain_inputs;
mod inputs;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;
mod verification_key;
mod wallet_inputs;
mod witness;

use core::fmt::Debug;
use std::error::Error;

pub use chain_inputs::{ZkTransferChainInputs, ZkTransferChainInputsData};
pub use inputs::{ZkTransferWitnessInputs, ZkTransferWitnessInputsData};
use lb_circuits_prover::Prover as _;
use lb_groth16::{
    CompressedGroth16Proof, Groth16Proof, Groth16ProofJsonDeser, groth16_batch_verify,
};
use lb_log_targets::proofs;
use tracing::error;
pub use wallet_inputs::{
    InputPathAndSelectors, TX_COMMITMENT_TREE_DEPTH, ZkTransferInputNoteData,
    ZkTransferOutputNoteData, ZkTransferWalletInputs, ZkTransferWalletInputsData,
};

pub use crate::inputs::{
    ZkTransferInputsError, ZkTransferVerifierInput, ZkTransferVerifierInputJson,
};

pub const MAX_INPUTS: usize = 4;
pub const MAX_OUTPUTS: usize = 8;

pub type ZkTransferProof = CompressedGroth16Proof;
pub type ProveError = lbp_error::Error;

const LOG_TARGET: &str = proofs::ZKTRANSFER;

///
/// This function generates a proof for the given set of inputs.
///
/// # Arguments
/// - `inputs`: A reference to `ZkTransferWitnessInputs`, which contains the
///   necessary data to generate the witness and construct the proof.
///
/// # Returns
/// - `Ok((ZkTransferProof, ZkTransferVerifierInput))`: On success, returns a
///   tuple containing the generated proof (`ZkTransferProof`) and the
///   corresponding public inputs (`ZkTransferVerifierInput`).
/// - `Err(ProveError)`: On failure, returns an error of type `ProveError`,
///   which can occur due to I/O errors or JSON (de)serialization errors.
///
/// # Errors
/// - Returns a `ProveError::Io` if an I/O error occurs while generating the
///   witness or proving from contents.
/// - Returns a `ProveError::Json` if there is an error during JSON
///   serialization or deserialization.
pub fn prove(
    inputs: ZkTransferWitnessInputs,
) -> Result<(ZkTransferProof, ZkTransferVerifierInput), ProveError> {
    let witness = witness::generate_witness(inputs)?;
    let result = lb_circuits_prover::Rapidsnark::prove(
        lbc_transfer_sys::artifacts::PROVING_KEY,
        witness.as_ref(),
    )?;
    let proof: Groth16ProofJsonDeser = serde_json::from_str(&result.proof)?;
    let verifier_inputs: ZkTransferVerifierInputJson =
        serde_json::from_str(&result.public_signals)?;
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
/// - `proof`: A reference to the proof (`ZkTransferProof`) that needs
///   verification.
/// - `public_inputs`: A reference to `ZkTransferVerifierInput`, which contains
///   the public inputs against which the proof is verified.
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
pub fn verify(
    proof: &ZkTransferProof,
    public_inputs: &ZkTransferVerifierInput,
) -> Result<bool, VerifyError> {
    let inputs = public_inputs.to_inputs();
    let expanded_proof = Groth16Proof::try_from(proof).map_err(|_| VerifyError::Expansion)?;
    lb_groth16::groth16_verify(
        verification_key::ZKTRANSFER_VK.as_ref(),
        &expanded_proof,
        &inputs,
    )
    .map_err(|e| VerifyError::ProofVerify(Box::new(e)))
}

pub fn batch_verify(
    proofs_and_inputs: &[(ZkTransferProof, ZkTransferVerifierInput)],
) -> Result<bool, VerifyError> {
    let inputs: Vec<Vec<_>> = proofs_and_inputs
        .iter()
        .map(|(_, pi)| pi.to_inputs().to_vec())
        .collect();

    let expanded_proofs: Vec<Groth16Proof> = proofs_and_inputs
        .iter()
        .map(|(p, _)| Groth16Proof::try_from(p).map_err(|_| VerifyError::Expansion))
        .collect::<Result<Vec<_>, _>>()?; // short-circuits on first failure

    Ok(groth16_batch_verify(
        verification_key::ZKTRANSFER_VK.as_ref(),
        &expanded_proofs,
        &inputs,
    ))
}

#[cfg(test)]
mod tests {
    use lb_groth16::{AdditiveGroup as _, Field as _, Fr, Groth16Input};

    use super::*;
    use crate::test_utils::sample_witness_data;

    // Sample: active inputs of 4978 and 2412, eight outputs of 917, so 7390 minus
    // 7336.
    const SAMPLE_EXCESS: u64 = 54;

    fn frs(values: &[Groth16Input]) -> Vec<Fr> {
        values.iter().map(|value| value.into_inner()).collect()
    }

    #[test]
    fn test_full_flow() {
        let (proof, inputs) = prove(sample_witness_data().into()).unwrap();
        assert!(verify(&proof, &inputs).unwrap());
        assert_eq!(inputs.excess_value.into_inner(), Fr::from(SAMPLE_EXCESS));
        assert_eq!(frs(&inputs.inputs[2..]), vec![Fr::ZERO; 2]);
        assert!(
            frs(&inputs.inputs[..2])
                .iter()
                .all(|nullifier| *nullifier != Fr::ZERO)
        );
    }

    #[test]
    fn test_verifier_input_from_lists_matches_the_prover() {
        let data = sample_witness_data();
        let (proof, inputs) = prove(data.clone().into()).unwrap();
        let rebuilt = ZkTransferVerifierInput::new(
            &frs(&inputs.inputs[..2]),
            &frs(&inputs.outputs),
            SAMPLE_EXCESS,
            data.chain.tx_commitment_root,
            data.chain.msg,
        )
        .unwrap();
        assert_eq!(rebuilt.to_inputs(), inputs.to_inputs());
        assert!(verify(&proof, &rebuilt).unwrap());
    }

    #[test]
    fn test_verifier_input_rejects_too_many_notes() {
        assert_eq!(
            ZkTransferVerifierInput::new(&[Fr::ONE; 5], &[], 0, Fr::ZERO, Fr::ZERO).unwrap_err(),
            ZkTransferInputsError::TooManyInputs(5)
        );
        assert_eq!(
            ZkTransferVerifierInput::new(&[], &[Fr::ONE; 9], 0, Fr::ZERO, Fr::ZERO).unwrap_err(),
            ZkTransferInputsError::TooManyOutputs(9)
        );
    }

    #[test]
    fn test_verify_rejects_another_msg() {
        let (proof, inputs) = prove(sample_witness_data().into()).unwrap();
        let rebound = ZkTransferVerifierInput {
            msg: Groth16Input::new(Fr::ONE),
            ..inputs
        };
        assert!(!verify(&proof, &rebound).unwrap());
    }

    #[test]
    fn test_batch_verify_pairs_each_pi_a_with_its_own_pi_b() {
        let data = sample_witness_data();
        let mut other = data.clone();
        other.chain.msg = Fr::ONE;
        let (first, first_inputs) = prove(data.into()).unwrap();
        let (second, second_inputs) = prove(other.into()).unwrap();
        assert!(batch_verify(&[(first, first_inputs), (second, second_inputs)]).unwrap());

        let first_swapped = ZkTransferProof::new(first.pi_a, second.pi_b, first.pi_c);
        let second_swapped = ZkTransferProof::new(second.pi_a, first.pi_b, second.pi_c);
        assert!(
            !batch_verify(&[
                (first_swapped, first_inputs),
                (second_swapped, second_inputs)
            ])
            .unwrap()
        );
    }
}
