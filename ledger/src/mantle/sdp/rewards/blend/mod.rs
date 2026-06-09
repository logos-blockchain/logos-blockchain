mod current_epoch;
mod target_epoch;

use std::{fmt::Debug, num::NonZeroU64};

use lb_blend_message::{
    encap::ProofsVerifier as ProofsVerifierTrait, reward::BlendingTokenEvaluation,
};
use lb_blend_proofs::quota::inputs::prove::public::LeaderInputs;
use lb_core::{
    blend::core_quota,
    mantle::{Utxo, Value},
    sdp::{ActivityMetadata, ProviderId, ServiceParameters},
};
use lb_utils::math::NonNegativeF64;

use crate::{
    EpochState,
    mantle::sdp::rewards::{
        Error,
        blend::{
            current_epoch::{CurrentEpochState, CurrentEpochTracker, CurrentEpochTrackerOutput},
            target_epoch::{TargetEpochState, TargetEpochTracker},
        },
    },
};

const LOG_TARGET: &str = "ledger::mantle::rewards::blend";

/// Tracks Blend rewards based on activity proofs submitted by providers.
/// Activity proofs for the epoch `E-1` must be submitted during epoch `E`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Rewards<ProofsVerifier> {
    /// State before the first target epoch is finalized, or if the target
    /// epoch has less than the minimum required number of declarations.
    /// No activity messages are accepted in this state.
    WithoutTargetEpoch {
        current_epoch_state: CurrentEpochState,
        current_epoch_tracker: CurrentEpochTracker,
    },
    /// State after a new target epoch `E-1` is finalized.
    /// This tracks activity proofs for the target epoch `E-1` submitted
    /// during the current epoch `E`.
    WithTargetEpoch {
        target_epoch_state: TargetEpochState<ProofsVerifier>,
        target_epoch_tracker: Box<TargetEpochTracker>,
        current_epoch_state: CurrentEpochState,
        current_epoch_tracker: CurrentEpochTracker,
    },
}

impl<ProofsVerifier> super::Rewards for Rewards<ProofsVerifier>
where
    ProofsVerifier: ProofsVerifierTrait + Clone + Debug + PartialEq + Send + Sync,
{
    type Params = RewardsParameters;

    fn update_active(
        &self,
        provider_id: ProviderId,
        metadata: &ActivityMetadata,
        params: &Self::Params,
    ) -> Result<Self, Error> {
        match self {
            Self::WithoutTargetEpoch { .. } => {
                // Reject all activity messages.
                Err(Error::TargetEpochNotSet)
            }
            Self::WithTargetEpoch {
                target_epoch_state,
                target_epoch_tracker,
                current_epoch_state,
                current_epoch_tracker,
            } => {
                let ActivityMetadata::Blend(proof) = metadata;

                let (zk_id, hamming_distance) = target_epoch_state.verify_proof(
                    &provider_id,
                    proof,
                    current_epoch_state,
                    params,
                )?;

                let target_epoch_tracker = target_epoch_tracker.insert(
                    provider_id,
                    target_epoch_state.epoch(),
                    zk_id,
                    hamming_distance,
                )?;

                Ok(Self::WithTargetEpoch {
                    target_epoch_state: target_epoch_state.clone(),
                    target_epoch_tracker: Box::new(target_epoch_tracker),
                    current_epoch_state: current_epoch_state.clone(),
                    current_epoch_tracker: current_epoch_tracker.clone(),
                })
            }
        }
    }

    fn update_epoch(
        &self,
        last_epoch_state: &EpochState,
        next_epoch_state: &EpochState,
        _config: &ServiceParameters,
        params: &Self::Params,
    ) -> (Self, Vec<Utxo>) {
        match self {
            Self::WithoutTargetEpoch {
                current_epoch_state,
                current_epoch_tracker,
            } => (
                Self::from_current_epoch_tracker_output(
                    current_epoch_tracker.finalize(
                        current_epoch_state,
                        last_epoch_state,
                        next_epoch_state,
                        params,
                    ),
                    TargetEpochTracker::new(),
                ),
                Vec::new(),
            ),
            Self::WithTargetEpoch {
                target_epoch_state,
                target_epoch_tracker,
                current_epoch_state,
                current_epoch_tracker,
            } => {
                let (target_epoch_tracker, rewards) =
                    target_epoch_tracker.finalize(target_epoch_state);

                let new_state = Self::from_current_epoch_tracker_output(
                    current_epoch_tracker.finalize(
                        current_epoch_state,
                        last_epoch_state,
                        next_epoch_state,
                        params,
                    ),
                    target_epoch_tracker,
                );

                (new_state, rewards)
            }
        }
    }

    fn add_income(&self, income: Value) -> Self {
        match self {
            Self::WithoutTargetEpoch {
                current_epoch_state,
                current_epoch_tracker,
            } => Self::WithoutTargetEpoch {
                current_epoch_state: current_epoch_state.clone(),
                current_epoch_tracker: current_epoch_tracker.add_block_rewards(income),
            },
            Self::WithTargetEpoch {
                target_epoch_state,
                target_epoch_tracker,
                current_epoch_state,
                current_epoch_tracker,
            } => Self::WithTargetEpoch {
                target_epoch_state: target_epoch_state.clone(),
                target_epoch_tracker: target_epoch_tracker.clone(),
                current_epoch_state: current_epoch_state.clone(),
                current_epoch_tracker: current_epoch_tracker.add_block_rewards(income),
            },
        }
    }
}

impl<ProofsVerifier> Rewards<ProofsVerifier> {
    /// Create a new uninitialized [`Rewards`] that doesn't accept activity
    /// messages until the first epoch update.
    #[must_use]
    pub fn new(settings: &RewardsParameters, epoch_state: &EpochState) -> Self {
        let current_epoch_state = CurrentEpochState::new(epoch_state, settings);
        let current_epoch_tracker = CurrentEpochTracker::new();
        Self::WithoutTargetEpoch {
            current_epoch_state,
            current_epoch_tracker,
        }
    }
}

impl<ProofsVerifier> Rewards<ProofsVerifier>
where
    ProofsVerifier: ProofsVerifierTrait + Clone + Debug + PartialEq + Send + Sync,
{
    fn from_current_epoch_tracker_output(
        current_epoch_output: CurrentEpochTrackerOutput<ProofsVerifier>,
        target_epoch_tracker: TargetEpochTracker,
    ) -> Self {
        match current_epoch_output {
            CurrentEpochTrackerOutput::WithTargetEpoch {
                target_epoch_state,
                current_epoch_state,
                current_epoch_tracker,
            } => Self::WithTargetEpoch {
                target_epoch_state,
                target_epoch_tracker: Box::new(target_epoch_tracker),
                current_epoch_state,
                current_epoch_tracker,
            },
            CurrentEpochTrackerOutput::WithoutTargetEpoch {
                current_epoch_state,
                current_epoch_tracker,
            } => Self::WithoutTargetEpoch {
                current_epoch_state,
                current_epoch_tracker,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RewardsParameters {
    pub rounds_per_epoch: NonZeroU64,
    pub message_frequency_per_round: NonNegativeF64,
    pub num_blend_layers: NonZeroU64,
    pub data_replication_factor: u64,
    pub minimum_network_size: NonZeroU64,
    pub activity_threshold_sensitivity: u64,
}

impl RewardsParameters {
    fn core_quota_and_token_evaluation(
        &self,
        num_core_nodes: u64,
    ) -> Result<(u64, BlendingTokenEvaluation), lb_blend_message::reward::Error> {
        let core_quota = core_quota(
            self.rounds_per_epoch,
            self.message_frequency_per_round,
            self.num_blend_layers,
            num_core_nodes as usize,
        );
        Ok((
            core_quota,
            BlendingTokenEvaluation::new(
                core_quota,
                num_core_nodes,
                self.activity_threshold_sensitivity,
            )?,
        ))
    }

    fn leader_inputs(&self, epoch_state: &EpochState) -> LeaderInputs {
        let num_blend_layers = self.num_blend_layers.get();
        let message_quota = num_blend_layers + (num_blend_layers * self.data_replication_factor);
        LeaderInputs {
            pol_ledger_aged: epoch_state.utxos.root(),
            pol_epoch_nonce: epoch_state.nonce,
            message_quota,
            lottery_0: epoch_state.lottery_0,
            lottery_1: epoch_state.lottery_1,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, convert::Infallible};

    use lb_blend_message::crypto::proofs::PoQVerificationInputsMinusSigningKey;
    use lb_blend_proofs::{
        quota::{ProofOfQuota, VerifiedProofOfQuota},
        selection::{ProofOfSelection, VerifiedProofOfSelection, inputs::VerifyInputs},
    };
    use lb_core::{
        crypto::ZkHash,
        sdp::{ServiceType, blend},
    };
    use lb_groth16::{Field as _, Fr};
    use lb_key_management_system_keys::keys::{Ed25519Key, Ed25519PublicKey};

    use super::*;
    use crate::mantle::sdp::rewards::{
        Rewards as _,
        test_utils::{create_epoch_state, create_provider_id, create_service_parameters},
    };

    fn create_blend_rewards_params(
        rounds_per_epoch: u64,
        minimum_network_size: u64,
    ) -> RewardsParameters {
        RewardsParameters {
            rounds_per_epoch: rounds_per_epoch.try_into().unwrap(),
            message_frequency_per_round: NonNegativeF64::try_from(1.0).unwrap(),
            num_blend_layers: NonZeroU64::new(3).unwrap(),
            minimum_network_size: minimum_network_size.try_into().unwrap(),
            data_replication_factor: 0,
            activity_threshold_sensitivity: 1,
        }
    }

    fn new_proof_of_quota_unchecked(byte: u8) -> ProofOfQuota {
        VerifiedProofOfQuota::from_bytes_unchecked([byte; _]).into()
    }

    fn new_signing_key(byte: u8) -> Ed25519PublicKey {
        Ed25519Key::from_bytes(&[byte; _]).public_key()
    }

    fn new_proof_of_selection_unchecked(byte: u8) -> ProofOfSelection {
        VerifiedProofOfSelection::from_bytes_unchecked([byte; _]).into()
    }

    #[test]
    fn test_blend_no_reward_calculated_after_epoch0() {
        // Create epoch0 with providers
        let params = create_blend_rewards_params(864_000, 1);
        let epoch0 = create_epoch_state(
            &[create_provider_id(1), create_provider_id(2)],
            ServiceType::BlendNetwork,
            0.into(),
            Fr::ZERO,
            &params,
        );

        // Create a reward tracker based on epoch0
        let rewards_tracker = Rewards::<AlwaysSuccessProofsVerifier>::new(&params, &epoch0);

        // Update epoch from 0 to 1
        let epoch1 = EpochState {
            epoch: 1.into(),
            nonce: Fr::ONE,
            ..epoch0.clone()
        };
        let (_, rewards) =
            rewards_tracker.update_epoch(&epoch0, &epoch1, &create_service_parameters(), &params);

        // No rewards should be returned yet because epoch0 just ended,
        // and the reward calculation for the epoch0 just began.
        assert_eq!(rewards.len(), 0);
    }

    #[test]
    fn test_rewards_with_no_activity_proofs() {
        // Create a reward tracker, and update epoch from 0 to 1.
        let config = create_service_parameters();
        let params = create_blend_rewards_params(864_000, 1);
        let epoch0 = create_epoch_state(
            &[create_provider_id(1), create_provider_id(2)],
            ServiceType::BlendNetwork,
            0.into(),
            Fr::ZERO,
            &params,
        );
        let epoch1 = EpochState {
            epoch: 1.into(),
            nonce: Fr::ONE,
            ..epoch0.clone()
        };
        let (rewards_tracker, _) = Rewards::<AlwaysSuccessProofsVerifier>::new(&params, &epoch0)
            .update_epoch(&epoch0, &epoch1, &config, &params);

        // Update epoch from 1 to 2 without any activity proofs submitted.
        let epoch2 = EpochState {
            epoch: 2.into(),
            nonce: ZkHash::from(2),
            ..epoch1.clone()
        };
        let (_, rewards) = rewards_tracker.update_epoch(&epoch1, &epoch2, &config, &params);
        assert_eq!(rewards.len(), 0);
    }

    #[test]
    fn test_rewards_calculation() {
        let provider1 = create_provider_id(1);
        let provider2 = create_provider_id(2);
        let provider3 = create_provider_id(3);
        let provider4 = create_provider_id(4);

        // Create a reward tracker, accumulate epoch income during epoch 0,
        // and update epoch from 0 to 1.
        let config = create_service_parameters();
        let params = create_blend_rewards_params(864_000, 1);
        let epoch0 = create_epoch_state(
            &[provider1, provider2, provider3, provider4],
            ServiceType::BlendNetwork,
            0.into(),
            Fr::ZERO,
            &params,
        );
        let epoch1 = EpochState {
            epoch: 1.into(),
            nonce: Fr::ONE,
            ..epoch0.clone()
        };
        let (rewards_tracker, _) = Rewards::<AlwaysSuccessProofsVerifier>::new(&params, &epoch0)
            .add_income(1000)
            .update_epoch(&epoch0, &epoch1, &config, &params);

        // provider1 submits an activity proof
        let rewards_tracker = rewards_tracker
            .update_active(
                provider1,
                &ActivityMetadata::Blend(Box::new(blend::ActivityProof {
                    epoch: 0.into(),
                    proof_of_quota: new_proof_of_quota_unchecked(1),
                    signing_key: new_signing_key(1),
                    proof_of_selection: new_proof_of_selection_unchecked(1),
                })),
                &params,
            )
            .unwrap();

        // provider2 submits an activity proof, which has the minimum
        // Hamming distance among all proofs.
        let rewards_tracker = rewards_tracker
            .update_active(
                provider2,
                &ActivityMetadata::Blend(Box::new(blend::ActivityProof {
                    epoch: 0.into(),
                    proof_of_quota: new_proof_of_quota_unchecked(2),
                    signing_key: new_signing_key(2),
                    proof_of_selection: new_proof_of_selection_unchecked(2),
                })),
                &params,
            )
            .unwrap();

        // provider3 submits an activity proof
        let rewards_tracker = rewards_tracker
            .update_active(
                provider3,
                // Use the same proof as provider1 just for testing
                &ActivityMetadata::Blend(Box::new(blend::ActivityProof {
                    epoch: 0.into(),
                    proof_of_quota: new_proof_of_quota_unchecked(1),
                    signing_key: new_signing_key(1),
                    proof_of_selection: new_proof_of_selection_unchecked(1),
                })),
                &params,
            )
            .unwrap();

        // provider4 doesn't submit an activity proof.

        // Update epoch from 1 to 2.
        let epoch2 = EpochState {
            epoch: 2.into(),
            nonce: ZkHash::from(2),
            ..epoch1.clone()
        };
        let (_, reward_utxos) = rewards_tracker.update_epoch(&epoch1, &epoch2, &config, &params);

        assert_eq!(reward_utxos.len(), 3); // except provider4

        let Rewards::WithTargetEpoch {
            target_epoch_state, ..
        } = rewards_tracker
        else {
            panic!("rewards_tracker should be in Initialized state");
        };
        let zk_id_to_provider_id = target_epoch_state
            .providers()
            .map(|(provider_id, (zk_id, _))| (*zk_id, *provider_id))
            .collect::<HashMap<_, _>>();
        let rewards: HashMap<ProviderId, u64> = reward_utxos
            .iter()
            .map(|utxo| {
                let provider_id = zk_id_to_provider_id
                    .get(&utxo.note.pk)
                    .expect("provider should exist");
                (*provider_id, utxo.note.value)
            })
            .collect();

        // Provider2 gets double rewards compared to provider1 and provider3.
        assert_eq!(
            *rewards.get(&provider2).unwrap(),
            rewards.get(&provider1).unwrap() * 2
        );
        assert_eq!(
            *rewards.get(&provider2).unwrap(),
            rewards.get(&provider3).unwrap() * 2
        );
        // Provider4 should get no rewards.
        assert_eq!(rewards.get(&provider4), None);
    }

    #[test]
    fn test_blend_duplicate_active_messages() {
        let provider1 = create_provider_id(1);

        // Create a reward tracker, and update epoch from 0 to 1.
        let config = create_service_parameters();
        let params = create_blend_rewards_params(864_000, 1);
        let epoch0 = create_epoch_state(
            &[provider1],
            ServiceType::BlendNetwork,
            0.into(),
            Fr::ZERO,
            &params,
        );
        let epoch1 = EpochState {
            epoch: 1.into(),
            nonce: Fr::ONE,
            ..epoch0.clone()
        };
        let (rewards_tracker, _) = Rewards::<AlwaysSuccessProofsVerifier>::new(&params, &epoch0)
            .update_epoch(&epoch0, &epoch1, &config, &params);

        // provider1 submits an activity proof.
        let rewards_tracker = rewards_tracker
            .update_active(
                provider1,
                &ActivityMetadata::Blend(Box::new(blend::ActivityProof {
                    epoch: 0.into(),
                    proof_of_quota: new_proof_of_quota_unchecked(1),
                    signing_key: new_signing_key(1),
                    proof_of_selection: new_proof_of_selection_unchecked(1),
                })),
                &params,
            )
            .unwrap();

        // provider1 submits another activity proof in the same epoch,
        // which should error.
        let err = rewards_tracker
            .update_active(
                provider1,
                &ActivityMetadata::Blend(Box::new(blend::ActivityProof {
                    epoch: 0.into(),
                    proof_of_quota: new_proof_of_quota_unchecked(2),
                    signing_key: new_signing_key(1),
                    proof_of_selection: new_proof_of_selection_unchecked(2),
                })),
                &params,
            )
            .unwrap_err();
        assert_eq!(
            err,
            Error::DuplicateActiveMessage {
                epoch: 0.into(),
                provider_id: Box::new(provider1)
            }
        );
    }

    #[test]
    fn test_blend_invalid_epoch() {
        let provider1 = create_provider_id(1);

        // Create a reward tracker, and update epoch from 0 to 1.
        let config = create_service_parameters();
        let params = create_blend_rewards_params(864_000, 1);
        let epoch0 = create_epoch_state(
            &[provider1],
            ServiceType::BlendNetwork,
            0.into(),
            Fr::ZERO,
            &params,
        );
        let epoch1 = EpochState {
            epoch: 1.into(),
            nonce: Fr::ONE,
            ..epoch0.clone()
        };
        let (rewards_tracker, _) = Rewards::<AlwaysSuccessProofsVerifier>::new(&params, &epoch0)
            .update_epoch(&epoch0, &epoch1, &config, &params);

        // provider1 submits an activity proof with invalid epoch.
        let err = rewards_tracker
            .update_active(
                provider1,
                &ActivityMetadata::Blend(Box::new(blend::ActivityProof {
                    epoch: 99.into(),
                    proof_of_quota: new_proof_of_quota_unchecked(1),
                    signing_key: new_signing_key(1),
                    proof_of_selection: new_proof_of_selection_unchecked(1),
                })),
                &params,
            )
            .unwrap_err();
        assert_eq!(
            err,
            Error::InvalidEpoch {
                expected: 0.into(),
                got: 99.into()
            }
        );

        // No reward should be calculated after epoch 1.
        let epoch2 = EpochState {
            epoch: 2.into(),
            nonce: ZkHash::from(2),
            ..epoch1.clone()
        };
        let (_, rewards) = rewards_tracker.update_epoch(&epoch1, &epoch2, &config, &params);
        assert_eq!(rewards.len(), 0);
    }

    #[test]
    fn test_blend_network_too_small() {
        let provider1 = create_provider_id(1);

        // Create a reward tracker, and update epoch from 0 to 1.
        let config = create_service_parameters();
        // Set minimum network size to 2
        let params = create_blend_rewards_params(864_000, 2);
        let epoch0 = create_epoch_state(
            &[provider1],
            ServiceType::BlendNetwork,
            0.into(),
            Fr::ZERO,
            &params,
        );
        let epoch1 = EpochState {
            epoch: 1.into(),
            nonce: Fr::ONE,
            ..epoch0.clone()
        };
        let (rewards_tracker, _) = Rewards::<AlwaysSuccessProofsVerifier>::new(&params, &epoch0)
            .update_epoch(&epoch0, &epoch1, &config, &params);

        // provider1 submits an activity proof, but it should be rejected
        // since the network is too small.
        let err = rewards_tracker
            .update_active(
                provider1,
                &ActivityMetadata::Blend(Box::new(blend::ActivityProof {
                    epoch: 0.into(),
                    proof_of_quota: new_proof_of_quota_unchecked(1),
                    signing_key: new_signing_key(1),
                    proof_of_selection: new_proof_of_selection_unchecked(1),
                })),
                &params,
            )
            .unwrap_err();
        assert_eq!(err, Error::TargetEpochNotSet);

        // No reward should be calculated after epoch 1.
        let epoch2 = EpochState {
            epoch: 2.into(),
            nonce: ZkHash::from(2),
            ..epoch1.clone()
        };
        let (_, rewards) = rewards_tracker.update_epoch(&epoch1, &epoch2, &config, &params);
        assert_eq!(rewards.len(), 0);
    }

    #[test]
    fn test_blend_proof_distance_larger_than_activity_threshold() {
        let provider1 = create_provider_id(1);

        // Create a reward tracker, and update epoch from 0 to 1.
        let config = create_service_parameters();
        let params = create_blend_rewards_params(10, 1);
        let epoch0 = create_epoch_state(
            &[provider1],
            ServiceType::BlendNetwork,
            0.into(),
            ZkHash::from(9999),
            &params,
        );
        let epoch1 = EpochState {
            epoch: 1.into(),
            nonce: Fr::ONE,
            ..epoch0.clone()
        };
        let (rewards_tracker, _) = Rewards::<AlwaysSuccessProofsVerifier>::new(&params, &epoch0)
            .update_epoch(&epoch0, &epoch1, &config, &params);

        // provider1 submits an activity proof that is larger than activity threshold.
        let err = rewards_tracker
            .update_active(
                provider1,
                &ActivityMetadata::Blend(Box::new(blend::ActivityProof {
                    epoch: 0.into(),
                    proof_of_quota: new_proof_of_quota_unchecked(2),
                    signing_key: new_signing_key(2),
                    proof_of_selection: new_proof_of_selection_unchecked(2),
                })),
                &params,
            )
            .unwrap_err();
        assert_eq!(err, Error::HammingDistanceTooLarge);

        // No reward should be calculated after epoch 1.
        let epoch2 = EpochState {
            epoch: 2.into(),
            nonce: ZkHash::from(2),
            ..epoch1.clone()
        };
        let (_, rewards) = rewards_tracker.update_epoch(&epoch1, &epoch2, &config, &params);
        assert_eq!(rewards.len(), 0);
    }

    #[test]
    fn test_blend_invalid_proofs() {
        let provider1 = create_provider_id(1);

        // Create a reward tracker, and update epoch from 0 to 1.
        let config = create_service_parameters();
        let params = create_blend_rewards_params(1000, 1);
        let epoch0 = create_epoch_state(
            &[provider1],
            ServiceType::BlendNetwork,
            0.into(),
            Fr::ZERO,
            &params,
        );
        let epoch1 = EpochState {
            epoch: 1.into(),
            nonce: Fr::ONE,
            ..epoch0.clone()
        };
        let (rewards_tracker, _) = Rewards::<AlwaysFailureProofsVerifier>::new(&params, &epoch0)
            .update_epoch(&epoch0, &epoch1, &config, &params);

        // provider1 submits an activity proof, but PoQ/PoSel verification fails.
        let err = rewards_tracker
            .update_active(
                provider1,
                &ActivityMetadata::Blend(Box::new(blend::ActivityProof {
                    epoch: 0.into(),
                    proof_of_quota: new_proof_of_quota_unchecked(1),
                    signing_key: new_signing_key(1),
                    proof_of_selection: new_proof_of_selection_unchecked(1),
                })),
                &params,
            )
            .unwrap_err();
        assert_eq!(err, Error::InvalidProof);

        // No reward should be calculated after epoch 1.
        let epoch2 = EpochState {
            epoch: 2.into(),
            nonce: ZkHash::from(2),
            ..epoch1.clone()
        };
        let (_, rewards) = rewards_tracker.update_epoch(&epoch1, &epoch2, &config, &params);
        assert_eq!(rewards.len(), 0);
    }

    #[test]
    fn test_blend_proof_verifier_after_epoch_update() {
        // Create a reward tracker, and update epoch from 0 to 1.
        let provider = create_provider_id(1);
        let config = create_service_parameters();
        let params = create_blend_rewards_params(1000, 1);
        let epoch0 = create_epoch_state(
            &[provider],
            ServiceType::BlendNetwork,
            0.into(),
            // Set 0 to epoch nonce, to make a proof verifier that always fails.
            Fr::ZERO,
            &params,
        );
        let epoch1 = EpochState {
            epoch: 1.into(),
            nonce: Fr::ONE,
            ..epoch0.clone()
        };
        let (rewards_tracker, _) = Rewards::<ZeroNonceFailureProofsVerifier>::new(&params, &epoch0)
            .update_epoch(&epoch0, &epoch1, &config, &params);

        // provider submits an activity proof, but rejected due to
        // ZeroNonceFailureProofsVerifier
        let err = rewards_tracker
            .update_active(
                provider,
                &ActivityMetadata::Blend(Box::new(blend::ActivityProof {
                    epoch: 0.into(),
                    proof_of_quota: new_proof_of_quota_unchecked(1),
                    signing_key: new_signing_key(1),
                    proof_of_selection: new_proof_of_selection_unchecked(1),
                })),
                &params,
            )
            .unwrap_err();
        assert_eq!(err, Error::InvalidProof);

        // Update epoch from 1 to 2
        let epoch2 = EpochState {
            epoch: 2.into(),
            nonce: ZkHash::from(2),
            ..epoch1.clone()
        };
        let (rewards_tracker, _) = rewards_tracker.update_epoch(&epoch1, &epoch2, &config, &params);

        // provider submits an activity proof again, and it should be accepted since the
        // proof verifier should be updated after epoch update.
        rewards_tracker
            .update_active(
                provider,
                &ActivityMetadata::Blend(Box::new(blend::ActivityProof {
                    epoch: 1.into(),
                    proof_of_quota: new_proof_of_quota_unchecked(1),
                    signing_key: new_signing_key(1),
                    proof_of_selection: new_proof_of_selection_unchecked(1),
                })),
                &params,
            )
            .unwrap();
    }

    #[derive(Debug, Clone, PartialEq)]
    struct AlwaysSuccessProofsVerifier;

    impl ProofsVerifierTrait for AlwaysSuccessProofsVerifier {
        type Error = Infallible;

        fn new(_public_inputs: PoQVerificationInputsMinusSigningKey) -> Self {
            Self
        }

        fn verify_proof_of_quota(
            &self,
            proof: ProofOfQuota,
            _signing_key: &Ed25519PublicKey,
        ) -> Result<VerifiedProofOfQuota, Self::Error> {
            Ok(VerifiedProofOfQuota::from_bytes_unchecked((&proof).into()))
        }

        fn verify_proof_of_selection(
            &self,
            proof: ProofOfSelection,
            _inputs: &VerifyInputs,
        ) -> Result<VerifiedProofOfSelection, Self::Error> {
            Ok(VerifiedProofOfSelection::from_bytes_unchecked(
                (&proof).into(),
            ))
        }
    }

    #[derive(Debug, Clone, PartialEq)]
    struct AlwaysFailureProofsVerifier;

    impl ProofsVerifierTrait for AlwaysFailureProofsVerifier {
        type Error = ();

        fn new(_public_inputs: PoQVerificationInputsMinusSigningKey) -> Self {
            Self
        }

        fn verify_proof_of_quota(
            &self,
            _proof: ProofOfQuota,
            _signing_key: &Ed25519PublicKey,
        ) -> Result<VerifiedProofOfQuota, Self::Error> {
            Err(())
        }

        fn verify_proof_of_selection(
            &self,
            _proof: ProofOfSelection,
            _inputs: &VerifyInputs,
        ) -> Result<VerifiedProofOfSelection, Self::Error> {
            Err(())
        }
    }

    #[derive(Debug, Clone, PartialEq)]
    struct ZeroNonceFailureProofsVerifier(bool);

    impl ProofsVerifierTrait for ZeroNonceFailureProofsVerifier {
        type Error = ();

        fn new(public_inputs: PoQVerificationInputsMinusSigningKey) -> Self {
            // Fail only if pol_epoch_nonce is ZERO
            Self(public_inputs.leader.pol_epoch_nonce != Fr::ZERO)
        }

        fn verify_proof_of_quota(
            &self,
            proof: ProofOfQuota,
            _signing_key: &Ed25519PublicKey,
        ) -> Result<VerifiedProofOfQuota, Self::Error> {
            if self.0 {
                Ok(VerifiedProofOfQuota::from_bytes_unchecked((&proof).into()))
            } else {
                Err(())
            }
        }

        fn verify_proof_of_selection(
            &self,
            proof: ProofOfSelection,
            _inputs: &VerifyInputs,
        ) -> Result<VerifiedProofOfSelection, Self::Error> {
            if self.0 {
                Ok(VerifiedProofOfSelection::from_bytes_unchecked(
                    (&proof).into(),
                ))
            } else {
                Err(())
            }
        }
    }
}
