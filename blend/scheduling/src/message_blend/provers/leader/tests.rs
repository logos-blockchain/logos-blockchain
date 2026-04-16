use core::time::Duration;

use lb_blend_proofs::selection::inputs::VerifyInputs;
use lb_cryptarchia_engine::Epoch;
use test_log::test;
use tokio::time::timeout;

use crate::message_blend::provers::{
    ProofsGeneratorSettings,
    leader::{LeaderProofsGenerator as _, RealLeaderProofsGenerator},
    test_utils::{
        poq_public_inputs_from_session_public_inputs_and_signing_key, valid_proof_of_leader_inputs,
    },
};

#[test(tokio::test)]
async fn proof_generation() {
    let leadership_quota = 15;
    let (public_inputs, private_inputs) = valid_proof_of_leader_inputs(leadership_quota);

    let mut leader_proofs_generator = RealLeaderProofsGenerator::new(
        ProofsGeneratorSettings {
            local_node_index: None,
            membership_size: 1,
            public_inputs,
            encapsulation_layers: 1.try_into().unwrap(),
            epoch: Epoch::new(0),
        },
        private_inputs,
    );

    for _ in 0..leadership_quota {
        let proof = leader_proofs_generator.get_next_proof().await;
        let verified_proof_of_quota = proof
            .proof_of_quota
            .into_inner()
            .verify(
                &poq_public_inputs_from_session_public_inputs_and_signing_key((
                    public_inputs,
                    proof.ephemeral_signing_key.public_key(),
                )),
            )
            .unwrap();
        proof
            .proof_of_selection
            .into_inner()
            .verify(&VerifyInputs {
                // Membership of 1 -> only a single index can be included
                expected_node_index: 0,
                key_nullifier: verified_proof_of_quota.key_nullifier(),
                total_membership_size: 1,
            })
            .unwrap();
    }

    // Next proof should still return `Some` since leadership proofs do not have a
    // maximum cap.
    timeout(
        Duration::from_secs(20),
        leader_proofs_generator.get_next_proof(),
    )
    .await
    .unwrap();
}

#[test(tokio::test)]
async fn epoch_rotation() {
    let leadership_quota = 15;
    let (public_inputs, private_inputs) = valid_proof_of_leader_inputs(leadership_quota);

    let mut leader_proofs_generator = RealLeaderProofsGenerator::new(
        ProofsGeneratorSettings {
            local_node_index: None,
            membership_size: 1,
            public_inputs,
            encapsulation_layers: 1.try_into().unwrap(),
            epoch: Epoch::new(0),
        },
        private_inputs,
    );

    let proof = leader_proofs_generator.get_next_proof().await;
    let verified_proof_of_quota = proof
        .proof_of_quota
        .into_inner()
        .verify(
            &poq_public_inputs_from_session_public_inputs_and_signing_key((
                public_inputs,
                proof.ephemeral_signing_key.public_key(),
            )),
        )
        .unwrap();
    proof
        .proof_of_selection
        .into_inner()
        .verify(&VerifyInputs {
            expected_node_index: 0,
            key_nullifier: verified_proof_of_quota.key_nullifier(),
            total_membership_size: 1,
        })
        .unwrap();

    // Generate and verify new proof.
    let proof = leader_proofs_generator.get_next_proof().await;
    let verified_proof_of_quota = proof
        .proof_of_quota
        .into_inner()
        .verify(
            &poq_public_inputs_from_session_public_inputs_and_signing_key((
                public_inputs,
                proof.ephemeral_signing_key.public_key(),
            )),
        )
        .unwrap();
    proof
        .proof_of_selection
        .into_inner()
        .verify(&VerifyInputs {
            expected_node_index: 0,
            key_nullifier: verified_proof_of_quota.key_nullifier(),
            total_membership_size: 1,
        })
        .unwrap();
}

/// Verify that calling `rotate_epoch` actually updates the epoch and
/// regenerates proofs, unlike the above test which only generates proofs
/// without rotating.
#[test(tokio::test)]
async fn rotate_epoch_updates_epoch_and_regenerates_proofs() {
    let leadership_quota = 15;
    let (public_inputs, private_inputs) = valid_proof_of_leader_inputs(leadership_quota);

    let mut leader_proofs_generator = RealLeaderProofsGenerator::new(
        ProofsGeneratorSettings {
            local_node_index: None,
            membership_size: 1,
            public_inputs,
            encapsulation_layers: 1.try_into().unwrap(),
            epoch: Epoch::new(0),
        },
        private_inputs.clone(),
    );

    // Generate one proof on epoch 0.
    drop(leader_proofs_generator.get_next_proof().await);
    assert_eq!(leader_proofs_generator.current_epoch(), Epoch::new(0));

    // Rotate to epoch 1 - this should update epoch and regenerate the proofs
    // stream.
    leader_proofs_generator.rotate_epoch(
        public_inputs.leader,
        private_inputs.clone(),
        Epoch::new(1),
    );
    assert_eq!(leader_proofs_generator.current_epoch(), Epoch::new(1));

    // Proofs should still be generated successfully after rotation.
    let proof = leader_proofs_generator.get_next_proof().await;
    proof
        .proof_of_quota
        .into_inner()
        .verify(
            &poq_public_inputs_from_session_public_inputs_and_signing_key((
                public_inputs,
                proof.ephemeral_signing_key.public_key(),
            )),
        )
        .unwrap();

    // Rotate to epoch 2.
    leader_proofs_generator.rotate_epoch(public_inputs.leader, private_inputs, Epoch::new(2));
    assert_eq!(leader_proofs_generator.current_epoch(), Epoch::new(2));

    // Proofs should still verify after a second rotation.
    let proof = timeout(
        Duration::from_secs(20),
        leader_proofs_generator.get_next_proof(),
    )
    .await
    .unwrap();
    proof
        .proof_of_quota
        .into_inner()
        .verify(
            &poq_public_inputs_from_session_public_inputs_and_signing_key((
                public_inputs,
                proof.ephemeral_signing_key.public_key(),
            )),
        )
        .unwrap();
}
