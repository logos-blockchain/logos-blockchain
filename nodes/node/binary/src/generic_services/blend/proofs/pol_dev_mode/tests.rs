use futures::future::ready;
use lb_blend::{
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
use lb_blend_service::ProofsVerifier as _;
use lb_core::crypto::ZkHash;
use lb_groth16::Field as _;
use lb_key_management_system_service::keys::{UnsecuredEd25519Key, UnsecuredZkKey};

use crate::generic_services::blend::{CoreProofsGenerator, ProofsVerifier};

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
    let verifier = ProofsVerifier::new(public_inputs);

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
    let verifier = ProofsVerifier::new(public_inputs);

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
    let verifier = ProofsVerifier::new(public_inputs);

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
    let verifier = ProofsVerifier::new(public_inputs);

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
