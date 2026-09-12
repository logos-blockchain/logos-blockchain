use educe::Educe;
use lb_blend_proofs::selection::{VerifiedProofOfSelection, inputs::VerifyInputs};
use lb_cryptarchia_engine::Epoch;
use serde::Serialize;
use tracing::debug;

use crate::{
    encap::ProofsVerifier as ProofsVerifierTrait,
    reward::{
        LOG_TARGET,
        epoch::{BlendingTokenEvaluation, EpochRandomness},
        token::{BlendingToken, HammingDistance},
    },
};

/// Why an activity proof was rejected.
#[derive(Debug)]
pub enum VerifyError<E> {
    /// The proof of selection or the proof of quota did not verify.
    Proof(E),
    /// The token's Hamming distance to the next epoch randomness exceeds the
    /// activity threshold.
    HammingDistanceTooLarge,
}

/// An activity proof for an epoch, made of the blending token
/// that has the smallest Hamming distance satisfying the activity threshold.
#[derive(Educe, Serialize)]
#[educe(Debug)]
pub struct ActivityProof {
    epoch: Epoch,
    #[educe(Debug(ignore))]
    token: BlendingToken,
}

impl ActivityProof {
    #[must_use]
    pub const fn new(epoch: Epoch, token: BlendingToken) -> Self {
        Self { epoch, token }
    }

    #[must_use]
    pub const fn epoch(&self) -> Epoch {
        self.epoch
    }

    #[must_use]
    pub const fn token(&self) -> &BlendingToken {
        &self.token
    }

    /// Runs the cheap half of the verification: the proof of selection (a
    /// `ChaCha20` draw and one Poseidon2 compression) and the activity
    /// threshold on the not-yet-verified token (one serialisation and two
    /// Blake2b hashes).
    ///
    /// Neither check needs the proof of quota to have been verified: the
    /// proof of selection binds to the key nullifier carried by the proof of
    /// quota bytes, and the Hamming distance is a function of the token bytes
    /// only. Callers run this before [`Self::verify_quota`], whose Groth16
    /// pairing check is three orders of magnitude more expensive, so that a
    /// message a genuine provider can fail on never reaches the pairing.
    pub fn verify_selection_and_evaluate<ProofsVerifier>(
        proof: &lb_core::sdp::blend::ActivityProof,
        verifier: &ProofsVerifier,
        node_index: u64,
        membership_size: u64,
        token_evaluation: &BlendingTokenEvaluation,
        next_epoch_randomness: EpochRandomness,
    ) -> Result<(VerifiedProofOfSelection, HammingDistance), VerifyError<ProofsVerifier::Error>>
    where
        ProofsVerifier: ProofsVerifierTrait,
    {
        let proof_of_selection = verifier
            .verify_proof_of_selection(
                proof.proof_of_selection,
                &VerifyInputs {
                    expected_node_index: node_index,
                    total_membership_size: membership_size,
                    key_nullifier: proof.proof_of_quota.key_nullifier(),
                },
            )
            .map_err(VerifyError::Proof)?;

        let hamming_distance = token_evaluation
            .evaluate_unverified(
                &proof.signing_key,
                &proof.proof_of_quota,
                &proof.proof_of_selection,
                next_epoch_randomness,
            )
            .ok_or(VerifyError::HammingDistanceTooLarge)?;

        Ok((proof_of_selection, hamming_distance))
    }

    /// Runs the expensive half of the verification, the proof-of-quota
    /// pairing check, and builds the activity proof from the verified parts.
    ///
    /// `proof_of_selection` is the output of
    /// [`Self::verify_selection_and_evaluate`] for the same `proof`.
    pub fn verify_quota<ProofsVerifier>(
        proof: &lb_core::sdp::blend::ActivityProof,
        verifier: &ProofsVerifier,
        proof_of_selection: VerifiedProofOfSelection,
    ) -> Result<Self, ProofsVerifier::Error>
    where
        ProofsVerifier: ProofsVerifierTrait,
    {
        let proof_of_quota =
            verifier.verify_proof_of_quota(proof.proof_of_quota, &proof.signing_key)?;

        Ok(Self::new(
            proof.epoch,
            BlendingToken::new(proof.signing_key, proof_of_quota, proof_of_selection),
        ))
    }

    /// Verifies an activity proof and evaluates its token, cheapest check
    /// first: [`Self::verify_selection_and_evaluate`], then
    /// [`Self::verify_quota`].
    pub fn verify_and_build<ProofsVerifier>(
        proof: &lb_core::sdp::blend::ActivityProof,
        verifier: &ProofsVerifier,
        node_index: u64,
        membership_size: u64,
        token_evaluation: &BlendingTokenEvaluation,
        next_epoch_randomness: EpochRandomness,
    ) -> Result<(Self, HammingDistance), VerifyError<ProofsVerifier::Error>>
    where
        ProofsVerifier: ProofsVerifierTrait,
    {
        let (proof_of_selection, hamming_distance) = Self::verify_selection_and_evaluate(
            proof,
            verifier,
            node_index,
            membership_size,
            token_evaluation,
            next_epoch_randomness,
        )?;

        let verified_proof =
            Self::verify_quota(proof, verifier, proof_of_selection).map_err(VerifyError::Proof)?;

        Ok((verified_proof, hamming_distance))
    }
}

/// Computes the activity threshold, which is the expected maximum Hamming
/// distance from any blending token in an epoch to the next epoch
/// randomness.
pub fn activity_threshold(
    token_count_bit_len: u64,
    network_size_bit_len: u64,
    // Sensitivity parameter to control the lottery winning conditions.
    activity_threshold_sensitivity: u64,
) -> HammingDistance {
    debug!(
        target: LOG_TARGET,
        "Calculating activity threshold: token_count_bit_len={token_count_bit_len}, network_size_repr_bit_len={network_size_bit_len}, activity_threshold_sensitivity={activity_threshold_sensitivity}"
    );

    token_count_bit_len
        .saturating_sub(network_size_bit_len)
        .saturating_sub(activity_threshold_sensitivity)
        .into()
}

impl From<&ActivityProof> for lb_core::sdp::blend::ActivityProof {
    fn from(proof: &ActivityProof) -> Self {
        Self {
            epoch: proof.epoch,
            signing_key: *proof.token.signing_key(),
            proof_of_quota: (*proof.token.proof_of_quota()).into(),
            proof_of_selection: (*proof.token.proof_of_selection()).into(),
        }
    }
}
