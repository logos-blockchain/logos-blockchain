use core::num::NonZeroU64;

use lb_blend_proofs::{
    quota::{ProofOfQuota, VerifiedProofOfQuota},
    selection::{ProofOfSelection, VerifiedProofOfSelection, inputs::VerifyInputs},
};
use lb_key_management_system_keys::keys::Ed25519PublicKey;

use crate::{
    crypto::proofs::PoQVerificationInputsMinusSigningKey,
    message::{
        blending_header::BLENDING_HEADER_ENCODED_SIZE, payload::PAYLOAD_ENCODED_SIZE,
        public_header::PUBLIC_HEADER_ENCODED_SIZE,
    },
};

pub mod decapsulated;
pub mod encapsulated;
pub mod validated;

#[cfg(test)]
mod tests;

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

/// The exact serialized size, in bytes, of any well-formed message with
/// `num_layers` encapsulation layers.
///
/// The wire format is fixed-size, so this is fully determined by the layer
/// count. Used by the network crate to gate received bytes and to size the
/// encode buffer.
#[must_use]
pub fn expected_serialized_len(num_layers: NonZeroU64) -> usize {
    let layers_len = (num_layers.get() as usize)
        .checked_mul(BLENDING_HEADER_ENCODED_SIZE)
        .expect("message encoded length overflow");
    PUBLIC_HEADER_ENCODED_SIZE
        .checked_add(layers_len)
        .and_then(|len| len.checked_add(PAYLOAD_ENCODED_SIZE))
        .expect("message encoded length overflow")
}
