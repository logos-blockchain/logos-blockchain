mod inputs;
mod private;
mod witness;

pub use inputs::ZkSignWitnessInputs;
use lb_circuits_prover::Prover as _;
use lb_groth16::{CompressedGroth16Proof, Groth16Proof, Groth16ProofJsonDeser};
use lb_log_targets::proofs;
pub use private::ZkSignPrivateKeysData;
use tracing::error;

use self::inputs::{ZkSignVerifierInputsJson, ZkSignVerifierInputsJsonTryFromError};
use crate::{ZkSignProof, ZkSignVerifierInputs};

const LOG_TARGET: &str = proofs::ZKSIGN;

#[derive(Debug, thiserror::Error)]
pub enum ProveError {
    #[error(transparent)]
    Prove(#[from] lbp_error::Error),
    #[error(transparent)]
    VerifierInputsJson(#[from] ZkSignVerifierInputsJsonTryFromError),
}

/// Generate a ZK-signature proof and its public inputs.
///
/// # Errors
/// Returns an error if witness generation, proving, or conversion of the
/// prover's output fails.
pub fn prove(
    inputs: ZkSignWitnessInputs,
) -> Result<(ZkSignProof, ZkSignVerifierInputs), ProveError> {
    let witness = witness::generate_witness(inputs)?;
    let result = lb_circuits_prover::Rapidsnark::prove(
        lbc_signature_sys::artifacts::PROVING_KEY,
        witness.as_ref(),
    )
    .map_err(lbp_error::Error::from)?;
    let proof: Groth16ProofJsonDeser =
        serde_json::from_str(&result.proof).map_err(lbp_error::Error::from)?;
    let verifier_inputs: ZkSignVerifierInputsJson =
        serde_json::from_str(&result.public_signals).map_err(lbp_error::Error::from)?;
    let proof: Groth16Proof = proof
        .try_into()
        .map_err(lbp_error::Error::Groth16JsonProof)?;
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
            .map_err(ProveError::VerifierInputsJson)?,
    ))
}

#[cfg(test)]
mod tests;
