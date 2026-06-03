use std::{cmp::Ordering, collections::HashMap, iter::once};

use lb_blend_message::{
    encap::ProofsVerifier as ProofsVerifierTrait,
    reward::{BlendingTokenEvaluation, HammingDistance},
};
use lb_core::{
    mantle::{Utxo, Value},
    sdp::{ProviderId, ServiceType},
};
use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::ZkPublicKey;
use rpds::{HashTrieMapSync, HashTrieSetSync};

use crate::mantle::sdp::rewards::{
    Error,
    blend::{RewardsParameters, current_epoch::CurrentEpochState},
    distribute_rewards,
};

/// The immutable state of the target epoch for which rewards are being
/// calculated. The target epoch is `E-1` if `E` is the current epoch.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TargetEpochState<ProofsVerifier> {
    /// The target epoch
    epoch: Epoch,
    /// The providers in the target epoch (their public keys and indices).
    providers: HashTrieMapSync<ProviderId, (ZkPublicKey, u64)>,
    /// Parameters for evaluating activity proofs in the target epoch
    token_evaluation: BlendingTokenEvaluation,
    /// Verifier for `PoQ` and `PoSel` in the target epoch.
    proof_verifier: ProofsVerifier,
    /// Epoch incomes stabilized from epoch `E-1`
    epoch_income: Value,
}

impl<ProofsVerifier> TargetEpochState<ProofsVerifier> {
    pub const fn new(
        epoch: Epoch,
        providers: HashTrieMapSync<ProviderId, (ZkPublicKey, u64)>,
        token_evaluation: BlendingTokenEvaluation,
        proof_verifier: ProofsVerifier,
        epoch_income: Value,
    ) -> Self {
        Self {
            epoch,
            providers,
            token_evaluation,
            proof_verifier,
            epoch_income,
        }
    }

    pub const fn epoch(&self) -> Epoch {
        self.epoch
    }

    pub const fn epoch_income(&self) -> Value {
        self.epoch_income
    }

    pub fn providers(&self) -> impl Iterator<Item = (&ProviderId, &(ZkPublicKey, u64))> {
        self.providers.iter()
    }

    fn num_providers(&self) -> u64 {
        self.providers
            .size()
            .try_into()
            .expect("number of providers must fit in u64")
    }
}

impl<ProofsVerifier> TargetEpochState<ProofsVerifier>
where
    ProofsVerifier: ProofsVerifierTrait,
{
    pub fn verify_proof(
        &self,
        provider_id: &ProviderId,
        proof: &lb_core::sdp::blend::ActivityProof,
        current_epoch_state: &CurrentEpochState,
        settings: &RewardsParameters,
    ) -> Result<(ZkPublicKey, HammingDistance), Error> {
        if proof.epoch != self.epoch {
            return Err(Error::InvalidEpoch {
                expected: self.epoch,
                got: proof.epoch,
            });
        }

        let num_providers = self.num_providers();
        assert!(
            num_providers >= settings.minimum_network_size.get(),
            "number of providers must be >= minimum_network_size"
        );
        let &(zk_id, index) = self
            .providers
            .get(provider_id)
            .ok_or_else(|| Error::UnknownProvider(Box::new(*provider_id)))?;

        let verified_proof = lb_blend_message::reward::ActivityProof::verify_and_build(
            proof,
            &self.proof_verifier,
            index,
            num_providers,
        )
        .map_err(|_| Error::InvalidProof)?;

        tracing::trace!(
            "Verifying activity proof {:?} with epoch randomness: {:?}",
            verified_proof.token().signing_key(),
            current_epoch_state.epoch_randomness()
        );
        let Some(hamming_distance) = self.token_evaluation.evaluate(
            verified_proof.token(),
            current_epoch_state.epoch_randomness(),
        ) else {
            return Err(Error::HammingDistanceTooLarge);
        };

        Ok((zk_id, hamming_distance))
    }
}

/// Tracks activity proofs submitted for the target epoch whose rewards are
/// being calculated. The target epoch is `E-1` if `E` is the current epoch.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TargetEpochTracker {
    /// Collecting proofs submitted by providers in the target epoch.
    submitted_proofs: HashTrieMapSync<ProviderId, (ZkPublicKey, HammingDistance)>,
    /// Tracking the minimum Hamming distance among submitted proofs.
    min_hamming_distance: MinHammingDistance,
}

impl Default for TargetEpochTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl TargetEpochTracker {
    pub fn new() -> Self {
        Self {
            submitted_proofs: HashTrieMapSync::new_sync(),
            min_hamming_distance: MinHammingDistance::new(),
        }
    }

    pub fn insert(
        &self,
        provider_id: ProviderId,
        epoch: Epoch,
        zk_id: ZkPublicKey,
        hamming_distance: HammingDistance,
    ) -> Result<Self, Error> {
        if self.submitted_proofs.contains_key(&provider_id) {
            return Err(Error::DuplicateActiveMessage {
                epoch,
                provider_id: Box::new(provider_id),
            });
        }
        Ok(Self {
            submitted_proofs: self
                .submitted_proofs
                .insert(provider_id, (zk_id, hamming_distance)),
            min_hamming_distance: self
                .min_hamming_distance
                .with_update(hamming_distance, provider_id),
        })
    }

    pub fn finalize<ProofsVerifier>(
        &self,
        target_epoch_state: &TargetEpochState<ProofsVerifier>,
    ) -> (Self, Vec<Utxo>) {
        if self.submitted_proofs.is_empty() {
            return (Self::new(), vec![]);
        }

        // Identify premium providers with the minimum Hamming distance
        let premium_providers = &self.min_hamming_distance.providers;

        // Calculate base reward
        let base_reward = target_epoch_state.epoch_income()
            / (self.submitted_proofs.size() as u64 + premium_providers.size() as u64);

        // Calculate reward for each provider
        let mut rewards = HashMap::new();
        for (provider_id, (zk_id, _)) in self.submitted_proofs.iter() {
            let reward = if premium_providers.contains(provider_id) {
                base_reward * 2
            } else {
                base_reward
            };
            rewards.insert(*zk_id, reward);
        }

        (
            Self::new(),
            distribute_rewards(
                rewards,
                target_epoch_state.epoch(),
                ServiceType::BlendNetwork,
            ),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MinHammingDistance {
    min_distance: HammingDistance,
    providers: HashTrieSetSync<ProviderId>,
}

impl MinHammingDistance {
    fn new() -> Self {
        Self {
            min_distance: HammingDistance::MAX,
            providers: HashTrieSetSync::new_sync(),
        }
    }

    /// Creates a new [`MinHammingDistance`] updated with the given distance and
    /// provider.
    fn with_update(&self, distance: HammingDistance, provider: ProviderId) -> Self {
        match distance.cmp(&self.min_distance) {
            Ordering::Less => Self {
                min_distance: distance,
                providers: once(provider).collect(),
            },
            Ordering::Equal => Self {
                min_distance: distance,
                providers: self.providers.insert(provider),
            },
            Ordering::Greater => self.clone(),
        }
    }
}
