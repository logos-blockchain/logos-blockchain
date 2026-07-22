mod activity;
mod epoch;
mod token;

use std::collections::HashSet;

pub use activity::ActivityProof;
pub use epoch::EpochInfo;
use lb_cryptarchia_engine::Epoch;
use lb_log_targets::blend;
use serde::{Deserialize, Serialize};
pub use token::{BlendingToken, HammingDistance};

pub use crate::reward::epoch::{BlendingTokenEvaluation, EpochRandomness, Error};

const LOG_TARGET: &str = blend::message::REWARD;

/// Holds blending tokens collected during a single epoch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpochBlendingTokenCollector {
    epoch: Epoch,
    token_evaluation: BlendingTokenEvaluation,
    tokens: HashSet<BlendingToken>,
}

impl EpochBlendingTokenCollector {
    #[must_use]
    pub fn new(epoch_info: &EpochInfo) -> Self {
        Self {
            epoch: epoch_info.epoch,
            token_evaluation: epoch_info.token_evaluation,
            tokens: HashSet::new(),
        }
    }

    pub fn collect(&mut self, token: BlendingToken) {
        self.tokens.insert(token);
    }

    #[must_use]
    pub fn rotate_epoch(
        self,
        new_epoch_info: &EpochInfo,
    ) -> (Self, OldEpochBlendingTokenCollector) {
        let new_collector = Self::new(new_epoch_info);
        let old_collector = OldEpochBlendingTokenCollector {
            collector: self,
            next_epoch_randomness: new_epoch_info.epoch_randomness(),
        };
        (new_collector, old_collector)
    }

    #[must_use]
    pub const fn epoch(&self) -> Epoch {
        self.epoch
    }

    #[cfg(any(test, feature = "unsafe-test-functions"))]
    #[must_use]
    pub const fn tokens(&self) -> &HashSet<BlendingToken> {
        &self.tokens
    }
}

/// Holds blending tokens collected during the old epoch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OldEpochBlendingTokenCollector {
    collector: EpochBlendingTokenCollector,
    next_epoch_randomness: EpochRandomness,
}

impl OldEpochBlendingTokenCollector {
    pub fn collect(&mut self, token: BlendingToken) {
        self.collector.collect(token);
    }

    /// Computes an activity proof for this epoch by consuming tokens.
    ///
    /// It returns `None` if there was no blending token satisfying the
    /// activity threshold calculated.
    #[must_use]
    pub fn compute_activity_proof(self) -> Option<ActivityProof> {
        // Find the blending token with the smallest Hamming distance,
        // which is <= activity threshold.
        tracing::trace!(
            LOG_TARGET,
            "Computing activity proof for epoch {} with activity threshold {:?} and next epoch randomness {:?} for {} candidate tokens",
            self.collector.epoch(),
            self.collector.token_evaluation.activity_threshold(),
            self.next_epoch_randomness,
            self.collector.tokens.len()
        );
        let (winning_activity_proof, distance) = self
            .collector
            .tokens
            .into_iter()
            .filter_map(|token| {
                let token_signing_key = *token.signing_key();

                let distance = self
                    .collector
                    .token_evaluation
                    .evaluate(&token, self.next_epoch_randomness)
                    .map(|distance| (token, distance));

                tracing::trace!(
                    LOG_TARGET,
                    "Evaluated token {token_signing_key:?} with distance {distance:?}",
                );

                distance
            })
            .min_by_key(|(_, distance)| *distance)
            .map(|(token, distance)| (ActivityProof::new(self.collector.epoch, token), distance))?;

        tracing::trace!(
            LOG_TARGET,
            "Computed activity proof for epoch {}: {winning_activity_proof:?} with Hamming distance {distance:?}",
            self.collector.epoch,
        );

        Some(winning_activity_proof)
    }

    #[must_use]
    pub const fn epoch(&self) -> Epoch {
        self.collector.epoch()
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
    use lb_blend_proofs::{
        quota::{PROOF_OF_QUOTA_SIZE, VerifiedProofOfQuota},
        selection::{PROOF_OF_SELECTION_SIZE, VerifiedProofOfSelection},
    };
    use lb_core::crypto::ZkHash;
    use lb_key_management_system_keys::keys::Ed25519Key;

    use super::*;

    #[test_log::test(test)]
    fn test_blending_token_collector() {
        let num_core_nodes = 2;
        let core_quota = 15;
        let epoch_info =
            EpochInfo::new(1.into(), &ZkHash::from(1), num_core_nodes, core_quota, 1).unwrap();
        let mut tokens = EpochBlendingTokenCollector::new(&epoch_info);
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

        // Prepare a new epoch info.
        let epoch_info =
            EpochInfo::new(2.into(), &ZkHash::from(2), num_core_nodes, core_quota, 1).unwrap();
        let (_, mut tokens) = tokens.rotate_epoch(&epoch_info);

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
