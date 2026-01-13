use const_hex::FromHex as _;
use groth16::{Field as _, fr_from_bytes_unchecked};
use nomos_core::codec::{DeserializeOp as _, SerializeOp as _};

use crate::{
    ZkHash,
    selection::{
        Error, KEY_NULLIFIER_DERIVATION_DOMAIN_SEPARATION_TAG_FR, ProofOfSelection,
        VerifiedProofOfSelection, derive_key_nullifier_from_secret_selection_randomness,
        inputs::VerifyInputs,
    },
};

#[test]
fn secret_selection_randomness_to_key_nullifier_dst_encoding() {
    // Blend spec: <https://www.notion.so/nomos-tech/Proof-of-Quota-Specification-215261aa09df81d88118ee22205cbafe?source=copy_link#26c261aa09df802f9dbcd0780dc7ac6e>

    assert_eq!(
        *KEY_NULLIFIER_DERIVATION_DOMAIN_SEPARATION_TAG_FR,
        fr_from_bytes_unchecked(
            &<[u8; 16]>::from_hex("0x4b45595f4e554c4c49464945525f5631").unwrap()
        )
    );
}

#[test]
fn success_on_valid_proof() {
    let posel = VerifiedProofOfSelection::new(ZkHash::ZERO).into_inner();
    let expected_key_nullifier =
        derive_key_nullifier_from_secret_selection_randomness(ZkHash::ZERO);
    posel
        .verify(&VerifyInputs {
            expected_node_index: 0,
            total_membership_size: 1,
            key_nullifier: expected_key_nullifier,
        })
        .unwrap();
}

#[test]
fn failure_on_invalid_nullifier() {
    let posel = VerifiedProofOfSelection::new(ZkHash::ZERO).into_inner();
    let Err(Error::KeyNullifierMismatch { expected, provided }) = posel.verify(&VerifyInputs {
        expected_node_index: 0,
        total_membership_size: 1,
        key_nullifier: ZkHash::ONE,
    }) else {
        panic!("`posel.verify` should fail.");
    };
    assert_eq!(
        expected,
        derive_key_nullifier_from_secret_selection_randomness(ZkHash::ZERO)
    );
    assert_eq!(provided, ZkHash::ONE);
}

#[test]
fn failure_on_invalid_index() {
    let posel = VerifiedProofOfSelection::new(ZkHash::ZERO).into_inner();
    let expected_index = posel.expected_index(2).unwrap();
    let Err(Error::IndexMismatch { expected, provided }) = posel.verify(&VerifyInputs {
        // We expect the opposite index.
        expected_node_index: 1 - expected_index as u64,
        total_membership_size: 2,
        key_nullifier: derive_key_nullifier_from_secret_selection_randomness(ZkHash::ZERO),
    }) else {
        panic!("posel.verify should fail.");
    };
    assert_eq!(expected.unwrap() as usize, expected_index);
    assert_eq!(provided, 1 - expected_index as u64);
}

#[test]
fn failure_on_expected_index_too_large() {
    let posel = VerifiedProofOfSelection::new(ZkHash::ZERO).into_inner();
    let Err(Error::IndexMismatch { expected, .. }) = posel.verify(&VerifyInputs {
        expected_node_index: 1,
        total_membership_size: 0,
        key_nullifier: ZkHash::ONE,
    }) else {
        panic!("`posel.verify` should fail.");
    };
    assert!(expected.is_none());
}

#[test]
fn failure_on_empty_membership() {
    let posel = VerifiedProofOfSelection::new(ZkHash::ZERO).into_inner();
    let Err(Error::EmptyMembershipSet) = posel.expected_index(0) else {
        panic!("`posel.verify` should fail.");
    };
}

#[test]
fn serde_verified_and_unverified() {
    let proof = VerifiedProofOfSelection::from_bytes_unchecked([100; _]);

    let serialized_proof = &proof.to_bytes().unwrap();

    let deserialized_proof_as_verified =
        VerifiedProofOfSelection::from_bytes(&serialized_proof[..]).unwrap();
    assert!(proof == deserialized_proof_as_verified);

    let deserialized_proof_as_unverified =
        ProofOfSelection::from_bytes(&serialized_proof[..]).unwrap();
    assert!(proof == deserialized_proof_as_unverified);
}
