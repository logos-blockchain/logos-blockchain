use derivative::Derivative;
use nomos_blend_proofs::selection::inputs::VerifyInputs;
use nomos_core::sdp::SessionNumber;
use serde::Serialize;
use tracing::debug;

use crate::{
    encap::ProofsVerifier as ProofsVerifierTrait,
    reward::{
        LOG_TARGET,
        token::{BlendingToken, HammingDistance},
    },
};

/// An activity proof for a session, made of the blending token
/// that has the smallest Hamming distance satisfying the activity threshold.
#[derive(Derivative, Serialize)]
#[derivative(Debug)]
pub struct ActivityProof {
    session_number: SessionNumber,
    #[derivative(Debug = "ignore")]
    token: BlendingToken,
}

impl ActivityProof {
    #[must_use]
    pub const fn new(session_number: SessionNumber, token: BlendingToken) -> Self {
        Self {
            session_number,
            token,
        }
    }

    #[must_use]
    pub const fn token(&self) -> &BlendingToken {
        &self.token
    }

    pub fn verify_and_build<ProofsVerifier>(
        proof: &nomos_core::sdp::blend::ActivityProof,
        verifier: &ProofsVerifier,
        node_index: u64,
        membership_size: u64,
    ) -> Result<Self, ProofsVerifier::Error>
    where
        ProofsVerifier: ProofsVerifierTrait,
    {
        let proof_of_quota =
            verifier.verify_proof_of_quota(proof.proof_of_quota, &proof.signing_key)?;
        let proof_of_selection = verifier.verify_proof_of_selection(
            proof.proof_of_selection,
            &VerifyInputs {
                expected_node_index: node_index,
                total_membership_size: membership_size,
                key_nullifier: proof.proof_of_quota.key_nullifier(),
            },
        )?;

        Ok(Self::new(
            proof.session,
            BlendingToken::new(proof.signing_key, proof_of_quota, proof_of_selection),
        ))
    }
}

/// Sensitivity parameter to control the lottery winning conditions.
const ACTIVITY_THRESHOLD_SENSITIVITY_PARAM: u64 = 1;

/// Computes the activity threshold, which is the expected maximum Hamming
/// distance from any blending token in a session to the next session
/// randomness.
pub fn activity_threshold(token_count_bit_len: u64, network_size_bit_len: u64) -> HammingDistance {
    debug!(
        target: LOG_TARGET,
        "Calculating activity threshold: token_count_bit_len={token_count_bit_len}, network_size_repr_bit_len={network_size_bit_len}"
    );

    token_count_bit_len
        .saturating_sub(network_size_bit_len)
        .saturating_sub(ACTIVITY_THRESHOLD_SENSITIVITY_PARAM)
        .into()
}

impl From<&ActivityProof> for nomos_core::sdp::blend::ActivityProof {
    fn from(proof: &ActivityProof) -> Self {
        Self {
            session: proof.session_number,
            signing_key: *proof.token.signing_key(),
            proof_of_quota: (*proof.token.proof_of_quota()).into(),
            proof_of_selection: (*proof.token.proof_of_selection()).into(),
        }
    }
}
