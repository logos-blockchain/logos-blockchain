use const_hex::FromHex as _;
use groth16::{Field as _, fr_from_bytes_unchecked};
use key_management_system_keys::keys::UnsecuredZkKey;
use nomos_blend_crypto::{ZkHash, merkle::MerkleTree};

use crate::{
    quota::{
        DOMAIN_SEPARATION_TAG_FR, ED25519_PUBLIC_KEY_SIZE, Ed25519PublicKey, VerifiedProofOfQuota,
        fixtures::{valid_proof_of_core_quota_inputs, valid_proof_of_leadership_quota_inputs},
        inputs::prove::{
            PrivateInputs, PublicInputs,
            private::ProofOfCoreQuotaInputs,
            public::{CoreInputs, LeaderInputs},
        },
    },
    selection::derive_key_nullifier_from_secret_selection_randomness,
};

#[test]
fn secret_selection_randomness_dst_encoding() {
    // Blend spec: <https://www.notion.so/nomos-tech/Proof-of-Quota-Specification-215261aa09df81d88118ee22205cbafe?source=copy_link#25e261aa09df802d87edfc54d1d60b80>
    assert_eq!(
        *DOMAIN_SEPARATION_TAG_FR,
        fr_from_bytes_unchecked(
            &<[u8; 23]>::from_hex("0x53454c454354494f4e5f52414e444f4d4e4553535f5631").unwrap()
        ),
    );
}

#[test]
fn valid_proof_of_core_quota() {
    let (public_inputs, private_inputs) = valid_proof_of_core_quota_inputs(
        Ed25519PublicKey::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE]).unwrap(),
        1,
    );

    let (proof, secret_selection_randomness) = VerifiedProofOfQuota::new(
        &public_inputs,
        PrivateInputs::new_proof_of_core_quota_inputs(0, private_inputs),
    )
    .unwrap();

    let verified_proof_of_quota = proof.into_inner().verify(&public_inputs).unwrap();
    assert_eq!(
        derive_key_nullifier_from_secret_selection_randomness(secret_selection_randomness),
        verified_proof_of_quota.key_nullifier()
    );
}

// We test that our assumption that two PoQs with the exact same public and
// private inputs but different ephemeral key still produce the same nullifier.
#[test]
fn same_key_nullifier_for_different_public_keys() {
    let key_1: Ed25519PublicKey =
        Ed25519PublicKey::from_bytes(&[200; ED25519_PUBLIC_KEY_SIZE]).unwrap();
    let key_2: Ed25519PublicKey =
        Ed25519PublicKey::from_bytes(&[250; ED25519_PUBLIC_KEY_SIZE]).unwrap();

    let (public_inputs_key_1, private_inputs_key_1) = valid_proof_of_core_quota_inputs(key_1, 1);
    let (public_inputs_key_2, private_inputs_key_2) = valid_proof_of_core_quota_inputs(key_2, 1);

    let (proof_key_1, _) = VerifiedProofOfQuota::new(
        &public_inputs_key_1,
        PrivateInputs::new_proof_of_core_quota_inputs(0, private_inputs_key_1),
    )
    .unwrap();
    let verified_proof_of_quota_1 = proof_key_1
        .into_inner()
        .verify(&public_inputs_key_1)
        .unwrap();
    let (proof_key_2, _) = VerifiedProofOfQuota::new(
        &public_inputs_key_2,
        PrivateInputs::new_proof_of_core_quota_inputs(0, private_inputs_key_2),
    )
    .unwrap();
    let verified_proof_of_quota_2 = proof_key_2
        .into_inner()
        .verify(&public_inputs_key_2)
        .unwrap();

    assert_eq!(
        verified_proof_of_quota_1.key_nullifier(),
        verified_proof_of_quota_2.key_nullifier()
    );
}

#[test]
fn valid_proof_of_leadership_quota() {
    let (public_inputs, private_inputs) = valid_proof_of_leadership_quota_inputs(
        Ed25519PublicKey::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE]).unwrap(),
        1,
    );

    let (proof, secret_selection_randomness) = VerifiedProofOfQuota::new(
        &public_inputs,
        PrivateInputs::new_proof_of_leadership_quota_inputs(0, private_inputs),
    )
    .unwrap();

    let verified_proof_of_quota = proof.into_inner().verify(&public_inputs).unwrap();
    assert_eq!(
        derive_key_nullifier_from_secret_selection_randomness(secret_selection_randomness),
        verified_proof_of_quota.key_nullifier()
    );
}

struct PoQInputs<const INPUTS: usize> {
    public_inputs: PublicInputs,
    secret_inputs: [ProofOfCoreQuotaInputs; INPUTS],
}

fn generate_inputs<const INPUTS: usize>() -> PoQInputs<INPUTS> {
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
            quota: 1,
            zk_root: merkle_tree.root(),
        };
        let leader_inputs = LeaderInputs {
            message_quota: 1,
            pol_epoch_nonce: ZkHash::ZERO,
            pol_ledger_aged: ZkHash::ZERO,
            total_stake: 1,
        };
        let session = 1;
        let signing_key = Ed25519PublicKey::from_bytes(&[10; ED25519_PUBLIC_KEY_SIZE]).unwrap();
        PublicInputs {
            core: core_inputs,
            leader: leader_inputs,
            session,
            signing_key,
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

#[test]
fn poq_interaction_single_key() {
    let PoQInputs {
        public_inputs,
        secret_inputs,
    } = generate_inputs::<1>();

    for secret_input in secret_inputs {
        let (poq, _) = VerifiedProofOfQuota::new(
            &public_inputs,
            PrivateInputs::new_proof_of_core_quota_inputs(0, secret_input),
        )
        .unwrap();
        poq.into_inner().verify(&public_inputs).unwrap();
    }
}

#[test]
fn poq_interaction_two_keys() {
    let PoQInputs {
        public_inputs,
        secret_inputs,
    } = generate_inputs::<2>();

    for secret_input in secret_inputs {
        let (poq, _) = VerifiedProofOfQuota::new(
            &public_inputs,
            PrivateInputs::new_proof_of_core_quota_inputs(0, secret_input),
        )
        .unwrap();
        poq.into_inner().verify(&public_inputs).unwrap();
    }
}

#[test]
fn poq_interaction_three_keys() {
    let PoQInputs {
        public_inputs,
        secret_inputs,
    } = generate_inputs::<3>();

    for secret_input in secret_inputs {
        let (poq, _) = VerifiedProofOfQuota::new(
            &public_inputs,
            PrivateInputs::new_proof_of_core_quota_inputs(0, secret_input),
        )
        .unwrap();
        poq.into_inner().verify(&public_inputs).unwrap();
    }
}

#[test]
fn poq_interaction_four_keys() {
    let PoQInputs {
        public_inputs,
        secret_inputs,
    } = generate_inputs::<3>();

    for secret_input in secret_inputs {
        let (poq, _) = VerifiedProofOfQuota::new(
            &public_inputs,
            PrivateInputs::new_proof_of_core_quota_inputs(0, secret_input),
        )
        .unwrap();
        poq.into_inner().verify(&public_inputs).unwrap();
    }
}

#[test]
fn poq_interaction_one_hundred_keys() {
    let PoQInputs {
        public_inputs,
        secret_inputs,
    } = generate_inputs::<100>();

    for secret_input in secret_inputs {
        let (poq, _) = VerifiedProofOfQuota::new(
            &public_inputs,
            PrivateInputs::new_proof_of_core_quota_inputs(0, secret_input),
        )
        .unwrap();
        poq.into_inner().verify(&public_inputs).unwrap();
    }
}
