mod current_session;
mod target_session;

use std::{fmt::Debug, num::NonZeroU64};

use nomos_blend_message::{
    encap::ProofsVerifier as ProofsVerifierTrait, reward::BlendingTokenEvaluation,
};
use nomos_blend_proofs::quota::inputs::prove::public::LeaderInputs;
use nomos_core::{
    blend::core_quota,
    block::BlockNumber,
    mantle::Utxo,
    sdp::{ActivityMetadata, ProviderId, ServiceParameters},
};
use nomos_utils::math::NonNegativeF64;

use crate::{
    EpochState,
    mantle::sdp::{
        SessionState,
        rewards::{
            Error,
            blend::{
                current_session::{
                    CurrentSessionState, CurrentSessionTracker, CurrentSessionTrackerOutput,
                },
                target_session::{TargetSessionState, TargetSessionTracker},
            },
        },
    },
};

const LOG_TARGET: &str = "ledger::mantle::rewards::blend";

/// Tracks Blend rewards based on activity proofs submitted by providers.
/// Activity proofs for the session `s-1` must be submitted during session `s`.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq)]
pub enum Rewards<ProofsVerifier> {
    /// State before the first target session is finalized, or if the target
    /// session has less than the minimum required number of declarations.
    /// No activity messages are accepted in this state.
    WithoutTargetSession {
        settings: RewardsParameters,
        current_session_tracker: CurrentSessionTracker,
    },
    /// State after a new target session `s-1` is finalized.
    /// This tracks activity proofs for the target session `s-1` submitted
    /// during the current session `s`.
    WithTargetSession {
        target_session_state: TargetSessionState<ProofsVerifier>,
        target_session_tracker: TargetSessionTracker,
        current_session_state: CurrentSessionState,
        current_session_tracker: CurrentSessionTracker,
        settings: RewardsParameters,
    },
}

impl<ProofsVerifier> super::Rewards for Rewards<ProofsVerifier>
where
    ProofsVerifier: ProofsVerifierTrait + Clone + Debug + PartialEq + Send + Sync,
{
    fn update_active(
        &self,
        provider_id: ProviderId,
        metadata: &ActivityMetadata,
        _block_number: BlockNumber,
    ) -> Result<Self, Error> {
        match self {
            Self::WithoutTargetSession { .. } => {
                // Reject all activity messages.
                Err(Error::TargetSessionNotSet)
            }
            Self::WithTargetSession {
                target_session_state,
                target_session_tracker,
                current_session_state,
                current_session_tracker,
                settings,
            } => {
                let ActivityMetadata::Blend(proof) = metadata else {
                    return Err(Error::InvalidProofType);
                };

                let (zk_id, hamming_distance) = target_session_state.verify_proof(
                    &provider_id,
                    proof,
                    current_session_state,
                    settings,
                )?;

                let target_session_tracker = target_session_tracker.insert(
                    provider_id,
                    target_session_state.session_number(),
                    zk_id,
                    hamming_distance,
                )?;

                Ok(Self::WithTargetSession {
                    target_session_state: target_session_state.clone(),
                    target_session_tracker,
                    current_session_state: current_session_state.clone(),
                    current_session_tracker: current_session_tracker.clone(),
                    settings: settings.clone(),
                })
            }
        }
    }

    fn update_session(
        &self,
        last_active: &SessionState,
        next_session_first_epoch_state: &EpochState,
        _config: &ServiceParameters,
    ) -> (Self, Vec<Utxo>) {
        match self {
            Self::WithoutTargetSession {
                settings,
                current_session_tracker,
            } => (
                Self::from_current_session_tracker_output(
                    current_session_tracker.finalize(
                        last_active,
                        next_session_first_epoch_state,
                        settings,
                    ),
                    TargetSessionTracker::new(),
                    settings.clone(),
                ),
                Vec::new(),
            ),
            Self::WithTargetSession {
                target_session_state,
                target_session_tracker,
                current_session_tracker,
                settings,
                ..
            } => {
                // TODO: Calculate base rewards when session_income is added to config
                // For now using placeholder value of 0
                let session_income = 0;

                let (target_session_tracker, rewards) = target_session_tracker
                    .finalize(target_session_state.session_number(), session_income);

                let new_state = Self::from_current_session_tracker_output(
                    current_session_tracker.finalize(
                        last_active,
                        next_session_first_epoch_state,
                        settings,
                    ),
                    target_session_tracker,
                    settings.clone(),
                );

                (new_state, rewards)
            }
        }
    }

    fn update_epoch(&self, epoch_state: &EpochState) -> Self {
        match self {
            Self::WithoutTargetSession {
                settings,
                current_session_tracker,
            } => Self::WithoutTargetSession {
                settings: settings.clone(),
                current_session_tracker: current_session_tracker
                    .collect_epoch(epoch_state, settings),
            },
            Self::WithTargetSession {
                target_session_state,
                target_session_tracker,
                current_session_state,
                current_session_tracker,
                settings,
            } => Self::WithTargetSession {
                target_session_state: target_session_state.clone(),
                target_session_tracker: target_session_tracker.clone(),
                current_session_state: current_session_state.clone(),
                current_session_tracker: current_session_tracker
                    .collect_epoch(epoch_state, settings),
                settings: settings.clone(),
            },
        }
    }
}

impl<ProofsVerifier> Rewards<ProofsVerifier> {
    /// Create a new uninitialized [`Rewards`] that doesn't accept activity
    /// messages until the first session update.
    #[must_use]
    pub fn new(settings: RewardsParameters, epoch_state: &EpochState) -> Self {
        let current_session_tracker = CurrentSessionTracker::new(epoch_state, &settings);
        Self::WithoutTargetSession {
            settings,
            current_session_tracker,
        }
    }
}

impl<ProofsVerifier> Rewards<ProofsVerifier>
where
    ProofsVerifier: ProofsVerifierTrait + Clone + Debug + PartialEq + Send + Sync,
{
    fn from_current_session_tracker_output(
        current_session_output: CurrentSessionTrackerOutput<ProofsVerifier>,
        target_session_tracker: TargetSessionTracker,
        settings: RewardsParameters,
    ) -> Self {
        match current_session_output {
            CurrentSessionTrackerOutput::WithTargetSession {
                target_session_state,
                current_session_state,
                current_session_tracker,
            } => Self::WithTargetSession {
                target_session_state,
                target_session_tracker,
                current_session_state,
                current_session_tracker,
                settings,
            },
            CurrentSessionTrackerOutput::WithoutTargetSession(current_session_tracker) => {
                Self::WithoutTargetSession {
                    settings,
                    current_session_tracker,
                }
            }
        }
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq)]
pub struct RewardsParameters {
    pub rounds_per_session: NonZeroU64,
    pub message_frequency_per_round: NonNegativeF64,
    pub num_blend_layers: NonZeroU64,
    pub minimum_network_size: NonZeroU64,
}

impl RewardsParameters {
    fn core_quota_and_token_evaluation(
        &self,
        num_core_nodes: u64,
    ) -> Result<(u64, BlendingTokenEvaluation), nomos_blend_message::reward::Error> {
        let core_quota = core_quota(
            self.rounds_per_session,
            self.message_frequency_per_round,
            self.num_blend_layers,
            num_core_nodes as usize,
        );
        Ok((
            core_quota,
            BlendingTokenEvaluation::new(core_quota, num_core_nodes)?,
        ))
    }

    fn leader_inputs(&self, epoch_state: &EpochState) -> LeaderInputs {
        LeaderInputs {
            pol_ledger_aged: epoch_state.utxos.root(),
            pol_epoch_nonce: epoch_state.nonce,
            message_quota: self.num_blend_layers.get(),
            total_stake: epoch_state.total_stake,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, convert::Infallible};

    use groth16::Field as _;
    use key_management_system_keys::keys::{Ed25519Key, Ed25519PublicKey};
    use nomos_blend_message::crypto::proofs::PoQVerificationInputsMinusSigningKey;
    use nomos_blend_proofs::{
        quota::{ProofOfQuota, VerifiedProofOfQuota},
        selection::{ProofOfSelection, VerifiedProofOfSelection, inputs::VerifyInputs},
    };
    use nomos_core::{
        crypto::ZkHash,
        sdp::{ServiceType, blend},
    };

    use super::*;
    use crate::mantle::sdp::rewards::{
        Rewards as _,
        test_utils::{
            create_provider_id, create_service_parameters, create_test_session_state,
            dummy_epoch_state, dummy_epoch_state_with,
        },
    };

    fn create_blend_rewards_params(
        rounds_per_session: u64,
        minimum_network_size: u64,
    ) -> RewardsParameters {
        RewardsParameters {
            rounds_per_session: rounds_per_session.try_into().unwrap(),
            message_frequency_per_round: NonNegativeF64::try_from(1.0).unwrap(),
            num_blend_layers: NonZeroU64::new(3).unwrap(),
            minimum_network_size: minimum_network_size.try_into().unwrap(),
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
    fn test_blend_no_reward_calculated_after_session_0() {
        // Create a reward tracker
        let epoch_state = dummy_epoch_state();
        let rewards_tracker = Rewards::<AlwaysSuccessProofsVerifier>::new(
            create_blend_rewards_params(864_000, 1),
            &epoch_state,
        );

        // Create session_0 with providers
        let session_0 = create_test_session_state(
            &[create_provider_id(1), create_provider_id(2)],
            ServiceType::BlendNetwork,
            0,
        );

        // Update session from 0 to 1
        let (_, rewards) = rewards_tracker.update_session(
            &session_0,
            &dummy_epoch_state(),
            &create_service_parameters(),
        );

        // No rewards should be returned yet because session0 just ended,
        // and the reward calculation for the session0 just began.
        assert_eq!(rewards.len(), 0);
    }

    #[test]
    fn test_rewards_with_no_activity_proofs() {
        // Create a reward tracker, and update session from 0 to 1.
        let config = create_service_parameters();
        let epoch_state = dummy_epoch_state();
        let (rewards_tracker, _) = Rewards::<AlwaysSuccessProofsVerifier>::new(
            create_blend_rewards_params(864_000, 1),
            &epoch_state,
        )
        .update_session(
            &create_test_session_state(
                &[create_provider_id(1), create_provider_id(2)],
                ServiceType::BlendNetwork,
                0,
            ),
            &epoch_state,
            &config,
        );

        // Update session from 1 to 2 without any activity proofs submitted.
        let (_, rewards) = rewards_tracker.update_session(
            &create_test_session_state(
                &[create_provider_id(1), create_provider_id(2)],
                ServiceType::BlendNetwork,
                1,
            ),
            &epoch_state,
            &config,
        );
        assert_eq!(rewards.len(), 0);
    }

    #[test]
    #[ignore = "TODO: Re-enable when session_income is implemented (currently hardcoded to 0)"]
    fn test_rewards_calculation() {
        let provider1 = create_provider_id(1);
        let provider2 = create_provider_id(2);
        let provider3 = create_provider_id(3);
        let provider4 = create_provider_id(4);

        // Create a reward tracker, and update session from 0 to 1.
        let config = create_service_parameters();
        let epoch_state = dummy_epoch_state();
        let (rewards_tracker, _) = Rewards::<AlwaysSuccessProofsVerifier>::new(
            create_blend_rewards_params(864_000, 1),
            &epoch_state,
        )
        .update_session(
            &create_test_session_state(
                &[provider1, provider2, provider3, provider4],
                ServiceType::BlendNetwork,
                0,
            ),
            &epoch_state,
            &config,
        );

        // provider1 submits an activity proof, which has the minimum
        // Hamming distance among all proofs.
        let rewards_tracker = rewards_tracker
            .update_active(
                provider1,
                &ActivityMetadata::Blend(Box::new(blend::ActivityProof {
                    session: 0,
                    proof_of_quota: new_proof_of_quota_unchecked(1),
                    signing_key: new_signing_key(1),
                    proof_of_selection: new_proof_of_selection_unchecked(1),
                })),
                config.session_duration,
            )
            .unwrap();

        // provider2 submits an activity proof.
        let rewards_tracker = rewards_tracker
            .update_active(
                provider2,
                &ActivityMetadata::Blend(Box::new(blend::ActivityProof {
                    session: 0,
                    proof_of_quota: new_proof_of_quota_unchecked(2),
                    signing_key: new_signing_key(2),
                    proof_of_selection: new_proof_of_selection_unchecked(2),
                })),
                config.session_duration,
            )
            .unwrap();

        // provider3 submits an activity proof, which has the minimum
        // Hamming distance among all proofs.
        let rewards_tracker = rewards_tracker
            .update_active(
                provider3,
                // Use the same proof as provider1 just for testing
                &ActivityMetadata::Blend(Box::new(blend::ActivityProof {
                    session: 0,
                    proof_of_quota: new_proof_of_quota_unchecked(1),
                    signing_key: new_signing_key(1),
                    proof_of_selection: new_proof_of_selection_unchecked(1),
                })),
                config.session_duration,
            )
            .unwrap();

        // provider4 doesn't submit an activity proof.

        // Update session from 1 to 2.
        let (_, reward_utxos) = rewards_tracker.update_session(
            &create_test_session_state(
                &[provider1, provider2, provider3, provider4],
                ServiceType::BlendNetwork,
                1,
            ),
            &epoch_state,
            &config,
        );

        assert_eq!(reward_utxos.len(), 3); // except provider4

        let Rewards::WithTargetSession {
            target_session_state,
            ..
        } = rewards_tracker
        else {
            panic!("rewards_tracker should be in Initialized state");
        };
        let zk_id_to_provider_id = target_session_state
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

        // Provider1 and provider3 should get double rewards compared to provider2.
        assert_eq!(
            *rewards.get(&provider1).unwrap(),
            rewards.get(&provider2).unwrap() * 2
        );
        assert_eq!(
            *rewards.get(&provider3).unwrap(),
            rewards.get(&provider2).unwrap() * 2
        );
        // Provider4 should get no rewards.
        assert_eq!(rewards.get(&provider4), None);
    }

    #[test]
    fn test_blend_duplicate_active_messages() {
        let provider1 = create_provider_id(1);

        // Create a reward tracker, and update session from 0 to 1.
        let config = create_service_parameters();
        let epoch_state = dummy_epoch_state();
        let (rewards_tracker, _) = Rewards::<AlwaysSuccessProofsVerifier>::new(
            create_blend_rewards_params(864_000, 1),
            &epoch_state,
        )
        .update_session(
            &create_test_session_state(&[provider1], ServiceType::BlendNetwork, 0),
            &epoch_state,
            &config,
        );

        // provider1 submits an activity proof.
        let rewards_tracker = rewards_tracker
            .update_active(
                provider1,
                &ActivityMetadata::Blend(Box::new(blend::ActivityProof {
                    session: 0,
                    proof_of_quota: new_proof_of_quota_unchecked(1),
                    signing_key: new_signing_key(1),
                    proof_of_selection: new_proof_of_selection_unchecked(1),
                })),
                config.session_duration,
            )
            .unwrap();

        // provider1 submits another activity proof in the same session,
        // which should error.
        let err = rewards_tracker
            .update_active(
                provider1,
                &ActivityMetadata::Blend(Box::new(blend::ActivityProof {
                    session: 0,
                    proof_of_quota: new_proof_of_quota_unchecked(2),
                    signing_key: new_signing_key(1),
                    proof_of_selection: new_proof_of_selection_unchecked(2),
                })),
                config.session_duration,
            )
            .unwrap_err();
        assert_eq!(
            err,
            Error::DuplicateActiveMessage {
                session: 0,
                provider_id: Box::new(provider1)
            }
        );
    }

    #[test]
    fn test_blend_invalid_session() {
        let provider1 = create_provider_id(1);

        // Create a reward tracker, and update session from 0 to 1.
        let config = create_service_parameters();
        let epoch_state = dummy_epoch_state();
        let (rewards_tracker, _) = Rewards::<AlwaysSuccessProofsVerifier>::new(
            create_blend_rewards_params(864_000, 1),
            &epoch_state,
        )
        .update_session(
            &create_test_session_state(&[provider1], ServiceType::BlendNetwork, 0),
            &epoch_state,
            &config,
        );

        // provider1 submits an activity proof with invalid session.
        let err = rewards_tracker
            .update_active(
                provider1,
                &ActivityMetadata::Blend(Box::new(blend::ActivityProof {
                    session: 99,
                    proof_of_quota: new_proof_of_quota_unchecked(1),
                    signing_key: new_signing_key(1),
                    proof_of_selection: new_proof_of_selection_unchecked(1),
                })),
                config.session_duration,
            )
            .unwrap_err();
        assert_eq!(
            err,
            Error::InvalidSession {
                expected: 0,
                got: 99
            }
        );

        // No reward should be calculated after session 1.
        let (_, rewards) = rewards_tracker.update_session(
            &create_test_session_state(&[provider1], ServiceType::BlendNetwork, 1),
            &epoch_state,
            &config,
        );
        assert_eq!(rewards.len(), 0);
    }

    #[test]
    fn test_blend_network_too_small() {
        let provider1 = create_provider_id(1);

        // Create a reward tracker, and update session from 0 to 1.
        let config = create_service_parameters();
        let epoch_state = dummy_epoch_state();
        let (rewards_tracker, _) = Rewards::<AlwaysSuccessProofsVerifier>::new(
            // Set minimum network size to 2
            create_blend_rewards_params(864_000, 2),
            &epoch_state,
        )
        .update_session(
            &create_test_session_state(&[provider1], ServiceType::BlendNetwork, 0),
            &epoch_state,
            &config,
        );

        // provider1 submits an activity proof, but it should be rejected
        // since the network is too small.
        let err = rewards_tracker
            .update_active(
                provider1,
                &ActivityMetadata::Blend(Box::new(blend::ActivityProof {
                    session: 0,
                    proof_of_quota: new_proof_of_quota_unchecked(1),
                    signing_key: new_signing_key(1),
                    proof_of_selection: new_proof_of_selection_unchecked(1),
                })),
                config.session_duration,
            )
            .unwrap_err();
        assert_eq!(err, Error::TargetSessionNotSet);

        // No reward should be calculated after session 1.
        let (_, rewards) = rewards_tracker.update_session(
            &create_test_session_state(&[provider1], ServiceType::BlendNetwork, 1),
            &epoch_state,
            &config,
        );
        assert_eq!(rewards.len(), 0);
    }

    #[test]
    fn test_blend_proof_distance_larger_than_activity_threshold() {
        let provider1 = create_provider_id(1);

        // Create a reward tracker, and update session from 0 to 1.
        let config = create_service_parameters();
        let epoch_state = dummy_epoch_state_with(0, 9999);
        let (rewards_tracker, _) = Rewards::<AlwaysSuccessProofsVerifier>::new(
            create_blend_rewards_params(10, 1),
            &epoch_state,
        )
        .update_session(
            &create_test_session_state(&[provider1], ServiceType::BlendNetwork, 0),
            &epoch_state,
            &config,
        );

        // provider1 submits an activity proof that is larger than activity threshold.
        let err = rewards_tracker
            .update_active(
                provider1,
                &ActivityMetadata::Blend(Box::new(blend::ActivityProof {
                    session: 0,
                    proof_of_quota: new_proof_of_quota_unchecked(2),
                    signing_key: new_signing_key(2),
                    proof_of_selection: new_proof_of_selection_unchecked(2),
                })),
                config.session_duration,
            )
            .unwrap_err();
        assert_eq!(err, Error::InvalidProof);

        // No reward should be calculated after session 1.
        let (_, rewards) = rewards_tracker.update_session(
            &create_test_session_state(&[provider1], ServiceType::BlendNetwork, 1),
            &epoch_state,
            &config,
        );
        assert_eq!(rewards.len(), 0);
    }

    #[test]
    fn test_blend_invalid_proofs() {
        let provider1 = create_provider_id(1);

        // Create a reward tracker, and update session from 0 to 1.
        let config = create_service_parameters();
        let epoch_state = dummy_epoch_state();
        let (rewards_tracker, _) = Rewards::<AlwaysFailureProofsVerifier>::new(
            create_blend_rewards_params(1000, 1),
            &epoch_state,
        )
        .update_session(
            &create_test_session_state(&[provider1], ServiceType::BlendNetwork, 1),
            &epoch_state,
            &config,
        );

        // provider1 submits an activity proof, but PoQ/PoSel verification fails.
        let err = rewards_tracker
            .update_active(
                provider1,
                &ActivityMetadata::Blend(Box::new(blend::ActivityProof {
                    session: 1,
                    proof_of_quota: new_proof_of_quota_unchecked(1),
                    signing_key: new_signing_key(1),
                    proof_of_selection: new_proof_of_selection_unchecked(1),
                })),
                config.session_duration,
            )
            .unwrap_err();
        assert_eq!(err, Error::InvalidProof);

        // No reward should be calculated after session 1.
        let (_, rewards) = rewards_tracker.update_session(
            &create_test_session_state(&[provider1], ServiceType::BlendNetwork, 2),
            &dummy_epoch_state(),
            &config,
        );
        assert_eq!(rewards.len(), 0);
    }

    #[test]
    fn test_blend_epoch_updates() {
        // Create a reward tracker, and update session from 0 to 1.
        let rewards_tracker = Rewards::<ZeroNonceFailureProofsVerifier>::new(
            create_blend_rewards_params(1000, 1),
            // Set 0 to epoch nonce, to make a proof verifier that always fails.
            &dummy_epoch_state_with(0, 0),
        );
        if let Rewards::WithoutTargetSession {
            current_session_tracker,
            ..
        } = &rewards_tracker
        {
            assert_eq!(current_session_tracker.epoch_count(), 1);
        } else {
            panic!("Should not be initialized yet")
        }

        // A new epoch received before a new session starts.
        // Set non-zero to epoch nonce, to make a proof verifier that always succeed.
        let new_epoch = dummy_epoch_state_with(1, 1);
        let rewards_tracker = rewards_tracker.update_epoch(&new_epoch);
        if let Rewards::WithoutTargetSession {
            current_session_tracker,
            ..
        } = &rewards_tracker
        {
            assert_eq!(current_session_tracker.epoch_count(), 2);
        } else {
            panic!("Should not be initialized yet")
        }

        // Update session from 0 to 1, with the same epoch state as the last one.
        let provider1 = create_provider_id(1);
        let config = create_service_parameters();
        let (rewards_tracker, _) = rewards_tracker.update_session(
            &create_test_session_state(&[provider1], ServiceType::BlendNetwork, 0),
            &new_epoch,
            &config,
        );
        if let Rewards::WithTargetSession {
            current_session_tracker,
            ..
        } = &rewards_tracker
        {
            assert_eq!(current_session_tracker.epoch_count(), 1);
        } else {
            panic!("Should not be uninitialized");
        }

        rewards_tracker
            .update_active(
                provider1,
                &ActivityMetadata::Blend(Box::new(blend::ActivityProof {
                    session: 0,
                    proof_of_quota: new_proof_of_quota_unchecked(1),
                    signing_key: new_signing_key(1),
                    proof_of_selection: new_proof_of_selection_unchecked(1),
                })),
                config.session_duration,
            )
            .expect("Proofs must be successfully verified");
    }

    #[derive(Debug, Clone, PartialEq)]
    struct AlwaysSuccessProofsVerifier;

    impl ProofsVerifierTrait for AlwaysSuccessProofsVerifier {
        type Error = Infallible;

        fn new(_public_inputs: PoQVerificationInputsMinusSigningKey) -> Self {
            Self
        }

        fn start_epoch_transition(&mut self, _new_pol_inputs: LeaderInputs) {}

        fn complete_epoch_transition(&mut self) {}

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

        fn start_epoch_transition(&mut self, _new_pol_inputs: LeaderInputs) {}

        fn complete_epoch_transition(&mut self) {}

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
            Self(public_inputs.leader.pol_epoch_nonce == ZkHash::ZERO)
        }

        fn start_epoch_transition(&mut self, _new_pol_inputs: LeaderInputs) {}

        fn complete_epoch_transition(&mut self) {}

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
