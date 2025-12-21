use async_trait::async_trait;
use groth16::{Field as _, fr_to_bytes};
use key_management_system_service::keys::{Ed25519PublicKey, UnsecuredEd25519Key};
use nomos_blend::{
    crypto::random_sized_bytes,
    message::crypto::{
        key_ext::Ed25519SecretKeyExt as _,
        proofs::{Error as InnerVerifierError, PoQVerificationInputsMinusSigningKey},
    },
    proofs::{
        quota::{
            ProofOfQuota, VerifiedProofOfQuota,
            inputs::prove::{private::ProofOfLeadershipQuotaInputs, public::LeaderInputs},
        },
        selection::{ProofOfSelection, VerifiedProofOfSelection, inputs::VerifyInputs},
    },
    scheduling::message_blend::{
        CoreProofOfQuotaGenerator,
        provers::{
            BlendLayerProof, ProofsGeneratorSettings,
            core::{CoreProofsGenerator as _, RealCoreProofsGenerator},
            core_and_leader::CoreAndLeaderProofsGenerator,
            leader::LeaderProofsGenerator,
        },
    },
};
use nomos_blend_service::{ProofsVerifier, RealProofsVerifier};
use nomos_core::{codec::DeserializeOp as _, crypto::ZkHash};
use poq::PoQProof;

const LOG_TARGET: &str = "node::blend::proofs";
const DUMMY_POQ_ZK_NULLIFIER: ZkHash = ZkHash::ZERO;

// TODO: Add actual PoL proofs once the required inputs are successfully fetched
// by the Blend service.
/// `PoQ` generator that runs the actual generation logic for core `PoQ` proofs,
/// while it always returns mocked proofs for leadership variants.
pub struct CoreProofsGenerator<CorePoQGenerator>(RealCoreProofsGenerator<CorePoQGenerator>);

#[async_trait]
impl<CorePoQGenerator> CoreAndLeaderProofsGenerator<CorePoQGenerator>
    for CoreProofsGenerator<CorePoQGenerator>
where
    CorePoQGenerator: CoreProofOfQuotaGenerator + Clone + Send + Sync + 'static,
{
    fn new(
        settings: ProofsGeneratorSettings,
        core_proof_of_quota_generator: CorePoQGenerator,
    ) -> Self {
        Self(RealCoreProofsGenerator::new(
            settings,
            core_proof_of_quota_generator,
        ))
    }

    fn rotate_epoch(&mut self, new_epoch_public: LeaderInputs) {
        self.0.rotate_epoch(new_epoch_public);
    }

    fn set_epoch_private(&mut self, _new_epoch_private: ProofOfLeadershipQuotaInputs) {
        tracing::trace!(target: LOG_TARGET, "Core proof generator still generates mocked leadership PoQ proofs, so epoch private info won't have any effects.");
    }

    async fn get_next_core_proof(&mut self) -> Option<BlendLayerProof> {
        tracing::debug!(target: LOG_TARGET, "Core PoQ proof requested.");
        self.0.get_next_proof().await
    }

    async fn get_next_leader_proof(&mut self) -> Option<BlendLayerProof> {
        tracing::debug!(target: LOG_TARGET, "Leadership PoQ proof requested. A mock one will be returned for now.");
        Some(random_proof())
    }
}

// TODO: Add actual PoL proofs once the required inputs are successfully fetched
// by the Blend service.
pub struct EdgeProofsGenerator;

#[async_trait]
impl LeaderProofsGenerator for EdgeProofsGenerator {
    fn new(
        _settings: ProofsGeneratorSettings,
        _private_inputs: ProofOfLeadershipQuotaInputs,
    ) -> Self {
        Self
    }

    fn rotate_epoch(
        &mut self,
        _new_epoch_public: LeaderInputs,
        _new_private_inputs: ProofOfLeadershipQuotaInputs,
    ) {
    }

    async fn get_next_proof(&mut self) -> BlendLayerProof {
        random_proof()
    }
}

// Randomly generates PoQ and PoSel from bytes until a valid combination of both
// is generated.
fn random_proof() -> BlendLayerProof {
    loop {
        let proof_random_bytes = random_sized_bytes::<{ size_of::<PoQProof>() }>();
        let poq_bytes: Vec<_> = fr_to_bytes(&DUMMY_POQ_ZK_NULLIFIER)
            .into_iter()
            .chain(proof_random_bytes)
            .collect();
        let Ok(proof_of_quota) = VerifiedProofOfQuota::from_bytes(&poq_bytes[..]) else {
            continue;
        };
        let Ok(proof_of_selection) = VerifiedProofOfSelection::from_bytes(
            &random_sized_bytes::<{ size_of::<ProofOfSelection>() }>()[..],
        ) else {
            continue;
        };
        return BlendLayerProof {
            ephemeral_signing_key: UnsecuredEd25519Key::generate_with_blake_rng(),
            proof_of_quota,
            proof_of_selection,
        };
    }
}

/// `PoQ` verifier that runs the actual verification logic for core `PoQ`
/// proofs, while it always returns `Ok` for leadership proofs.
// TODO: Add actual PoL verifier once the verification inputs are successfully
// fetched by the Blend service.
#[derive(Clone)]
pub struct BlendProofsVerifier(RealProofsVerifier);

impl ProofsVerifier for BlendProofsVerifier {
    type Error = InnerVerifierError;

    fn new(public_inputs: PoQVerificationInputsMinusSigningKey) -> Self {
        Self(RealProofsVerifier::new(public_inputs))
    }

    fn start_epoch_transition(&mut self, new_pol_inputs: LeaderInputs) {
        self.0.start_epoch_transition(new_pol_inputs);
    }

    fn complete_epoch_transition(&mut self) {
        self.0.complete_epoch_transition();
    }

    #[expect(clippy::cognitive_complexity, reason = "Tracing macros.")]
    fn verify_proof_of_quota(
        &self,
        proof: ProofOfQuota,
        signing_key: &Ed25519PublicKey,
    ) -> Result<VerifiedProofOfQuota, Self::Error> {
        let key_nullifier = proof.key_nullifier();
        tracing::debug!(target: LOG_TARGET, "Verifying PoQ with key nullifier: {key_nullifier:?}");
        if proof.key_nullifier() == DUMMY_POQ_ZK_NULLIFIER {
            tracing::debug!(target: LOG_TARGET, "Mocked PoL PoQ proof received (automatically verified successfully).");
            Ok(VerifiedProofOfQuota::from_proof_of_quota_unchecked(proof))
        } else {
            tracing::debug!(target: LOG_TARGET, "Core PoQ proof received.");
            let verification_result = self.0.verify_proof_of_quota(proof, signing_key).inspect_err(|e| {
                tracing::debug!(target: LOG_TARGET, "Core PoQ proof with key nullifier {key_nullifier:?} verification failed with error {e:?}");
            })?;
            tracing::debug!(target: LOG_TARGET, "Core PoQ proof with key nullifier {key_nullifier:?} verified successfully.");
            Ok(verification_result)
        }
    }

    #[expect(clippy::cognitive_complexity, reason = "Tracing macros.")]
    fn verify_proof_of_selection(
        &self,
        proof: ProofOfSelection,
        inputs: &VerifyInputs,
    ) -> Result<VerifiedProofOfSelection, Self::Error> {
        let key_nullifier = inputs.key_nullifier;
        tracing::debug!(target: LOG_TARGET, "Verifying PoSel for key nullifier: {key_nullifier:?}");
        if inputs.key_nullifier == DUMMY_POQ_ZK_NULLIFIER {
            tracing::debug!(target: LOG_TARGET, "Mocked PoL PoSel proof received (automatically verified successfully).");
            Ok(VerifiedProofOfSelection::from_proof_of_selection_unchecked(
                proof,
            ))
        } else {
            tracing::debug!(target: LOG_TARGET, "Core PoSel proof received.");
            let verified_proof_of_selection = self.0.verify_proof_of_selection(proof, inputs).inspect_err(|e| {
                tracing::debug!(target: LOG_TARGET, "Core PoSel proof for key nullifier {key_nullifier:?} verification failed with error {e:?}");
            })?;
            tracing::debug!(target: LOG_TARGET, "Core PoSel proof for key nullifier {key_nullifier:?} verified successfully.");
            Ok(verified_proof_of_selection)
        }
    }
}

#[cfg(test)]
mod core_to_core_tests {
    use futures::future::ready;
    use groth16::Field as _;
    use key_management_system_service::keys::{UnsecuredEd25519Key, UnsecuredZkKey};
    use nomos_blend::{
        crypto::merkle::MerkleTree,
        message::crypto::{
            key_ext::Ed25519SecretKeyExt as _,
            proofs::{Error as VerifierError, PoQVerificationInputsMinusSigningKey},
        },
        proofs::{
            quota::{
                self, VerifiedProofOfQuota,
                inputs::prove::{
                    PrivateInputs, PublicInputs,
                    private::ProofOfCoreQuotaInputs,
                    public::{CoreInputs, LeaderInputs},
                },
            },
            selection::{self, inputs::VerifyInputs},
        },
        scheduling::message_blend::{
            CoreProofOfQuotaGenerator,
            provers::{
                BlendLayerProof, ProofsGeneratorSettings,
                core_and_leader::CoreAndLeaderProofsGenerator as _,
            },
        },
    };
    use nomos_blend_service::ProofsVerifier as _;
    use nomos_core::crypto::ZkHash;

    use crate::generic_services::blend::{BlendProofsVerifier, CoreProofsGenerator};

    struct PoQInputs<const INPUTS: usize> {
        public_inputs: PoQVerificationInputsMinusSigningKey,
        secret_inputs: [ProofOfCoreQuotaInputs; INPUTS],
    }

    fn generate_inputs<const INPUTS: usize>(core_quota: u64) -> PoQInputs<INPUTS> {
        let keys: [_; INPUTS] = (1..=INPUTS as u64)
            .map(|i| {
                let sk = UnsecuredZkKey::new(ZkHash::from(i));
                let pk = sk.to_public_key();
                (sk, pk)
            })
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        let merkle_tree =
            MerkleTree::new(keys.clone().map(|(_, pk)| pk.into_inner()).to_vec()).unwrap();
        let public_inputs = {
            let core_inputs = CoreInputs {
                quota: core_quota,
                zk_root: merkle_tree.root(),
            };
            let leader_inputs = LeaderInputs {
                message_quota: 1,
                pol_epoch_nonce: ZkHash::ZERO,
                pol_ledger_aged: ZkHash::ZERO,
                total_stake: 1,
            };
            let session = 1;
            PoQVerificationInputsMinusSigningKey {
                core: core_inputs,
                leader: leader_inputs,
                session,
            }
        };
        let secret_inputs = keys.map(|(sk, pk)| {
            let proof = merkle_tree.get_proof_for_key(pk.as_fr()).unwrap();
            ProofOfCoreQuotaInputs {
                core_sk: sk.into_inner(),
                core_path_and_selectors: proof,
            }
        });

        PoQInputs {
            public_inputs,
            secret_inputs,
        }
    }

    #[derive(Clone)]
    struct PoQGeneratorWithPrivateInfo(ProofOfCoreQuotaInputs);

    impl PoQGeneratorWithPrivateInfo {
        fn new(private_info: ProofOfCoreQuotaInputs) -> Self {
            Self(private_info)
        }
    }

    impl CoreProofOfQuotaGenerator for PoQGeneratorWithPrivateInfo {
        fn generate_poq(
            &self,
            public_inputs: &PublicInputs,
            key_index: u64,
        ) -> impl Future<Output = Result<(VerifiedProofOfQuota, ZkHash), quota::Error>> + Send + Sync
        {
            ready(VerifiedProofOfQuota::new(
                public_inputs,
                PrivateInputs::new_proof_of_core_quota_inputs(key_index, self.0.clone()),
            ))
        }
    }

    #[test_log::test(tokio::test)]
    async fn correct_core_proof_generation_and_verification() {
        const MEMBERSHIP_SIZE: usize = 2;

        let PoQInputs {
            public_inputs,
            secret_inputs,
        } = generate_inputs::<MEMBERSHIP_SIZE>(1);
        let mut first_generator = CoreProofsGenerator::new(
            ProofsGeneratorSettings {
                local_node_index: Some(0),
                membership_size: MEMBERSHIP_SIZE,
                public_inputs,
            },
            PoQGeneratorWithPrivateInfo::new(secret_inputs[0].clone()),
        );
        let mut second_generator = CoreProofsGenerator::new(
            ProofsGeneratorSettings {
                local_node_index: Some(1),
                membership_size: MEMBERSHIP_SIZE,
                public_inputs,
            },
            PoQGeneratorWithPrivateInfo::new(secret_inputs[1].clone()),
        );
        let verifier = BlendProofsVerifier::new(public_inputs);

        // Node `0` generates a core proof.
        let BlendLayerProof {
            ephemeral_signing_key,
            proof_of_quota,
            proof_of_selection,
        } = first_generator.get_next_core_proof().await.unwrap();

        // `PoQ` must be valid.
        let verified_proof_of_quota = verifier
            .verify_proof_of_quota(
                proof_of_quota.into_inner(),
                &ephemeral_signing_key.public_key(),
            )
            .unwrap();

        // With the test inputs, `PoSel` will be addressed to node `1`.
        assert!(matches!(
            verifier.verify_proof_of_selection(
                proof_of_selection.into_inner(),
                &VerifyInputs {
                    expected_node_index: 0,
                    key_nullifier: verified_proof_of_quota.key_nullifier(),
                    total_membership_size: MEMBERSHIP_SIZE as u64,
                }
            ),
            Err(VerifierError::ProofOfSelection(
                selection::Error::IndexMismatch {
                    expected: Some(1),
                    provided: 0
                }
            ))
        ));
        assert_eq!(
            verifier
                .verify_proof_of_selection(
                    proof_of_selection.into_inner(),
                    &VerifyInputs {
                        expected_node_index: 1,
                        key_nullifier: verified_proof_of_quota.key_nullifier(),
                        total_membership_size: MEMBERSHIP_SIZE as u64,
                    }
                )
                .unwrap(),
            proof_of_selection
        );

        // Node `1` generates a core proof.
        let BlendLayerProof {
            ephemeral_signing_key,
            proof_of_quota,
            proof_of_selection,
        } = second_generator.get_next_core_proof().await.unwrap();

        // `PoQ` must be valid.
        let verified_proof_of_quota = verifier
            .verify_proof_of_quota(
                proof_of_quota.into_inner(),
                &ephemeral_signing_key.public_key(),
            )
            .unwrap();

        // With the test inputs, `PoSel` will be directed to node `0`.
        assert!(matches!(
            verifier.verify_proof_of_selection(
                proof_of_selection.into_inner(),
                &VerifyInputs {
                    expected_node_index: 1,
                    key_nullifier: verified_proof_of_quota.key_nullifier(),
                    total_membership_size: MEMBERSHIP_SIZE as u64,
                }
            ),
            Err(VerifierError::ProofOfSelection(
                selection::Error::IndexMismatch {
                    expected: Some(0),
                    provided: 1
                }
            ))
        ));
        assert_eq!(
            verifier
                .verify_proof_of_selection(
                    proof_of_selection.into_inner(),
                    &VerifyInputs {
                        expected_node_index: 0,
                        key_nullifier: verified_proof_of_quota.key_nullifier(),
                        total_membership_size: MEMBERSHIP_SIZE as u64,
                    }
                )
                .unwrap(),
            proof_of_selection
        );
    }

    #[test_log::test(tokio::test)]
    async fn invalid_core_poq_detection() {
        const MEMBERSHIP_SIZE: usize = 2;

        let PoQInputs {
            public_inputs,
            secret_inputs,
        } = generate_inputs::<MEMBERSHIP_SIZE>(1);
        let mut generator = CoreProofsGenerator::new(
            ProofsGeneratorSettings {
                local_node_index: Some(0),
                membership_size: MEMBERSHIP_SIZE,
                public_inputs: PoQVerificationInputsMinusSigningKey {
                    // We change session number to generate invalid `PoQ` proofs.
                    session: u64::MAX,
                    ..public_inputs
                },
            },
            PoQGeneratorWithPrivateInfo::new(secret_inputs[0].clone()),
        );
        let verifier = BlendProofsVerifier::new(public_inputs);

        // Node `0` generates a core proof.
        let BlendLayerProof {
            ephemeral_signing_key,
            proof_of_quota,
            ..
        } = generator.get_next_core_proof().await.unwrap();

        // `PoQ` must be invalid.
        assert!(matches!(
            verifier.verify_proof_of_quota(
                proof_of_quota.into_inner(),
                &ephemeral_signing_key.public_key()
            ),
            Err(VerifierError::ProofOfQuota(quota::Error::InvalidProof))
        ));
    }

    #[test_log::test(tokio::test)]
    async fn invalid_core_posel_detection() {
        const MEMBERSHIP_SIZE: usize = 2;

        let PoQInputs {
            public_inputs,
            secret_inputs,
        } = generate_inputs::<MEMBERSHIP_SIZE>(1);
        let mut generator = CoreProofsGenerator::new(
            ProofsGeneratorSettings {
                local_node_index: Some(0),
                membership_size: MEMBERSHIP_SIZE,
                public_inputs,
            },
            PoQGeneratorWithPrivateInfo::new(secret_inputs[0].clone()),
        );
        let verifier = BlendProofsVerifier::new(public_inputs);

        // Node `0` generates a core proof.
        let BlendLayerProof {
            ephemeral_signing_key,
            proof_of_quota,
            proof_of_selection,
        } = generator.get_next_core_proof().await.unwrap();

        // `PoQ` must be valid.
        verifier
            .verify_proof_of_quota(
                proof_of_quota.into_inner(),
                &ephemeral_signing_key.public_key(),
            )
            .unwrap();
        // `PoSel` must be invalid since we change membership size, which results in a
        // different index than expected.
        assert!(matches!(
            verifier.verify_proof_of_selection(
                proof_of_selection.into_inner(),
                &VerifyInputs {
                    expected_node_index: 0,
                    total_membership_size: (MEMBERSHIP_SIZE + 1) as u64,
                    key_nullifier: ZkHash::ONE
                }
            ),
            Err(VerifierError::ProofOfSelection(
                selection::Error::IndexMismatch {
                    expected: Some(1),
                    provided: 0
                }
            ))
        ));
    }

    #[test_log::test(tokio::test)]
    async fn mock_leadership_generation_and_verification() {
        const MEMBERSHIP_SIZE: usize = 2;

        let PoQInputs {
            public_inputs,
            secret_inputs,
        } = generate_inputs::<MEMBERSHIP_SIZE>(1);
        let mut generator = CoreProofsGenerator::new(
            ProofsGeneratorSettings {
                local_node_index: Some(0),
                membership_size: MEMBERSHIP_SIZE,
                public_inputs,
            },
            PoQGeneratorWithPrivateInfo::new(secret_inputs[0].clone()),
        );
        let verifier = BlendProofsVerifier::new(public_inputs);

        let BlendLayerProof {
            proof_of_quota,
            proof_of_selection,
            ..
        } = generator.get_next_leader_proof().await.unwrap();

        // Using a random key still verifies the mock leader `PoQ` proof correctly.
        let verified_proof = verifier
            .verify_proof_of_quota(
                proof_of_quota.into_inner(),
                &UnsecuredEd25519Key::generate_with_blake_rng().public_key(),
            )
            .unwrap();
        assert_eq!(verified_proof.key_nullifier(), ZkHash::ZERO);
        // Using a random expected index, the mock leader `PoSel` proof still verifies
        // correctly.
        verifier
            .verify_proof_of_selection(
                proof_of_selection.into_inner(),
                &VerifyInputs {
                    expected_node_index: u64::MAX,
                    total_membership_size: 0,
                    key_nullifier: verified_proof.key_nullifier(),
                },
            )
            .unwrap();
    }
}
