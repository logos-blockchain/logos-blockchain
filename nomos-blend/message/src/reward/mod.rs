mod activity;
mod session;
mod token;

use std::collections::HashSet;

pub use activity::ActivityProof;
use nomos_core::sdp::SessionNumber;
use serde::{Deserialize, Serialize};
pub use session::SessionInfo;
pub use token::{BlendingToken, HammingDistance};

pub use crate::reward::session::{BlendingTokenEvaluation, Error, SessionRandomness};

const LOG_TARGET: &str = "blend::message::reward";

/// Holds blending tokens collected during a single session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionBlendingTokenCollector {
    session_number: SessionNumber,
    token_evaluation: BlendingTokenEvaluation,
    tokens: HashSet<BlendingToken>,
}

impl SessionBlendingTokenCollector {
    #[must_use]
    pub fn new(session_info: &SessionInfo) -> Self {
        Self {
            session_number: session_info.session_number,
            token_evaluation: session_info.token_evaluation,
            tokens: HashSet::new(),
        }
    }

    pub fn collect(&mut self, token: BlendingToken) {
        self.tokens.insert(token);
    }

    #[must_use]
    pub fn rotate_session(
        self,
        new_session_info: &SessionInfo,
    ) -> (Self, OldSessionBlendingTokenCollector) {
        let new_collector = Self::new(new_session_info);
        let old_collector = OldSessionBlendingTokenCollector {
            collector: self,
            next_session_randomness: new_session_info.session_randomness(),
        };
        (new_collector, old_collector)
    }

    #[must_use]
    pub const fn session_number(&self) -> SessionNumber {
        self.session_number
    }

    #[cfg(any(test, feature = "unsafe-test-functions"))]
    #[must_use]
    pub const fn tokens(&self) -> &HashSet<BlendingToken> {
        &self.tokens
    }
}

/// Holds blending tokens collected during the old session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OldSessionBlendingTokenCollector {
    collector: SessionBlendingTokenCollector,
    next_session_randomness: SessionRandomness,
}

impl OldSessionBlendingTokenCollector {
    pub fn collect(&mut self, token: BlendingToken) {
        self.collector.collect(token);
    }

    /// Computes an activity proof for this session by consuming tokens.
    ///
    /// It returns `None` if there was no blending token satisfying the
    /// activity threshold calculated.
    #[must_use]
    pub fn compute_activity_proof(self) -> Option<ActivityProof> {
        // Find the blending token with the smallest Hamming distance,
        // which is <= activity threshold.
        self.collector
            .tokens
            .into_iter()
            .filter_map(|token| {
                self.collector
                    .token_evaluation
                    .evaluate(&token, self.next_session_randomness)
                    .map(|distance| (token, distance))
            })
            .min_by_key(|(_, distance)| *distance)
            .map(|(token, _)| ActivityProof::new(self.collector.session_number, token))
    }

    #[must_use]
    pub const fn session_number(&self) -> SessionNumber {
        self.collector.session_number()
    }

    #[cfg(any(test, feature = "unsafe-test-functions"))]
    #[must_use]
    pub const fn tokens(&self) -> &HashSet<BlendingToken> {
        self.collector.tokens()
    }
}

#[must_use]
pub const fn evaluate_hamming_distance(distance: u64, activity_threshold: u64) -> bool {
    distance <= activity_threshold
}

#[cfg(test)]
mod tests {
    use key_management_system_keys::keys::Ed25519Key;
    use nomos_blend_proofs::{
        quota::{PROOF_OF_QUOTA_SIZE, VerifiedProofOfQuota},
        selection::{PROOF_OF_SELECTION_SIZE, VerifiedProofOfSelection},
    };
    use nomos_core::crypto::ZkHash;

    use super::*;

    #[test_log::test(test)]
    fn test_blending_token_collector() {
        let num_core_nodes = 2;
        let core_quota = 15;
        let session_info =
            SessionInfo::new(1, &ZkHash::from(1), num_core_nodes, core_quota).unwrap();
        let mut tokens = SessionBlendingTokenCollector::new(&session_info);
        assert!(tokens.tokens().is_empty());

        // Insert `total_core_quota-1` tokens.
        let total_core_quota = core_quota.checked_mul(num_core_nodes).unwrap();
        let mut i = 0;
        for _ in 0..(total_core_quota.checked_sub(1).unwrap()) {
            let signing_key: u8 = i.try_into().unwrap();
            let proof: u8 = i.try_into().unwrap();
            let token = blending_token(signing_key, proof, proof);
            tokens.collect(token.clone());
            assert!(tokens.tokens().contains(&token));
            i += 1;
        }

        // Prepare a new session info.
        let session_info =
            SessionInfo::new(2, &ZkHash::from(2), num_core_nodes, core_quota).unwrap();
        let (_, mut tokens) = tokens.rotate_session(&session_info);

        // Insert one more tokens.
        // Now,`total_core_quota` tokens have been collected.
        // So, we can expect that always one of them can be picked as an activity proof.
        let signing_key: u8 = i.try_into().unwrap();
        let proof: u8 = i.try_into().unwrap();
        let token = blending_token(signing_key, proof, proof);
        tokens.collect(token.clone());
        assert!(tokens.tokens().contains(&token));

        // Compute an activity proof
        let candidates = tokens.tokens().clone();
        let proof = tokens.compute_activity_proof().unwrap();
        assert!(candidates.contains(proof.token()));
    }

    fn blending_token(
        signing_key: u8,
        proof_of_quota: u8,
        proof_of_selection: u8,
    ) -> BlendingToken {
        BlendingToken::new(
            Ed25519Key::from_bytes(&[signing_key; _]).public_key(),
            VerifiedProofOfQuota::from_bytes_unchecked([proof_of_quota; PROOF_OF_QUOTA_SIZE]),
            VerifiedProofOfSelection::from_bytes_unchecked(
                [proof_of_selection; PROOF_OF_SELECTION_SIZE],
            ),
        )
    }
}
