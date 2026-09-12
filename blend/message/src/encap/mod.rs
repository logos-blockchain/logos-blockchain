use lb_blend_proofs::{
    quota::{ProofOfQuota, VerifiedProofOfQuota},
    selection::{ProofOfSelection, VerifiedProofOfSelection, inputs::VerifyInputs},
};
use lb_key_management_system_keys::keys::Ed25519PublicKey;

use crate::crypto::proofs::PoQVerificationInputsMinusSigningKey;

pub mod decapsulated;
pub mod encapsulated;
pub mod validated;

#[cfg(any(test, feature = "unsafe-test-functions"))]
mod scripted_verifier;
#[cfg(test)]
mod tests;

#[cfg(any(test, feature = "unsafe-test-functions"))]
pub use scripted_verifier::{
    ProofOfQuotaScript, ProofOfSelectionScript, Rejected, ScriptedProofsVerifier,
    VerificationCounts,
};

/// An epoch-bound `PoQ` verifier.
pub trait ProofsVerifier {
    type Error;

    /// Create a new proof verifier with the public inputs corresponding to the
    /// current epoch.
    fn new(public_inputs: PoQVerificationInputsMinusSigningKey) -> Self;

    /// Proof of Quota verification logic.
    fn verify_proof_of_quota(
        &self,
        proof: ProofOfQuota,
        signing_key: &Ed25519PublicKey,
    ) -> Result<VerifiedProofOfQuota, Self::Error>;

    /// Proof of Selection verification logic.
    fn verify_proof_of_selection(
        &self,
        proof: ProofOfSelection,
        inputs: &VerifyInputs,
    ) -> Result<VerifiedProofOfSelection, Self::Error>;
}
