use lb_blend_proofs::selection::inputs::VerifyInputs;
use lb_cryptarchia_engine::Epoch;
use test_log::test;

use crate::message_blend::provers::{
    ProofsGeneratorSettings,
    core_and_leader::{CoreAndLeaderProofsGenerator as _, RealCoreAndLeaderProofsGenerator},
    test_utils::{
        CorePoQGeneratorFromPrivateCoreQuotaInputs,
        poq_public_inputs_from_session_public_inputs_and_signing_key, valid_proof_of_leader_inputs,
        valid_proof_of_quota_inputs,
    },
};

#[test(tokio::test)]
async fn proof_generation() {
    let core_quota = 10;
    let (core_public_inputs, core_private_inputs) = valid_proof_of_quota_inputs(core_quota);

    let mut core_and_leader_proofs_generator = RealCoreAndLeaderProofsGenerator::new(
        ProofsGeneratorSettings {
            local_node_index: None,
            membership_size: 1,
            public_inputs: core_public_inputs,
            encapsulation_layers: 1.try_into().unwrap(),
            epoch: Epoch::new(0),
        },
        CorePoQGeneratorFromPrivateCoreQuotaInputs::new(core_private_inputs),
    );

    for _ in 0..core_quota {
        let proof = core_and_leader_proofs_generator
            .get_next_core_proof()
            .await
            .unwrap();
        let verified_proof_of_quota = proof
            .proof_of_quota
            .into_inner()
            .verify(
                &poq_public_inputs_from_session_public_inputs_and_signing_key((
                    core_public_inputs,
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

    // Next proof should be `None` since we ran out of core quota.
    assert!(
        core_and_leader_proofs_generator
            .get_next_core_proof()
            .await
            .is_none()
    );

    let leadership_quota = 15;
    let (leadership_public_inputs, leadership_private_inputs) =
        valid_proof_of_leader_inputs(leadership_quota);

    // We override all the settings since we fixtures for core and leadership proofs
    // use a different set of public inputs.
    core_and_leader_proofs_generator.override_settings(ProofsGeneratorSettings {
        local_node_index: None,
        membership_size: 1,
        public_inputs: leadership_public_inputs,
        encapsulation_layers: 1.try_into().unwrap(),
        epoch: Epoch::new(0),
    });
    core_and_leader_proofs_generator.set_epoch_private(
        leadership_private_inputs,
        leadership_public_inputs.leader,
        Epoch::new(1),
    );

    for _ in 0..leadership_quota {
        let proof = core_and_leader_proofs_generator
            .get_next_leader_proof()
            .await
            .unwrap();
        let verified_proof_of_quota = proof
            .proof_of_quota
            .into_inner()
            .verify(
                &poq_public_inputs_from_session_public_inputs_and_signing_key((
                    leadership_public_inputs,
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
}

#[test(tokio::test)]
async fn epoch_rotation() {
    let core_quota = 10;
    let (public_inputs, private_inputs) = valid_proof_of_quota_inputs(core_quota);

    let mut core_and_leader_proofs_generator = RealCoreAndLeaderProofsGenerator::new(
        ProofsGeneratorSettings {
            local_node_index: None,
            membership_size: 1,
            public_inputs,
            encapsulation_layers: 1.try_into().unwrap(),
            epoch: Epoch::new(1),
        },
        CorePoQGeneratorFromPrivateCoreQuotaInputs::new(private_inputs),
    );

    // Request all but the last proof, before rotating epoch (with the same public
    // data because proofs use hard-coded fixtures).
    for _ in 0..(core_quota - 1) {
        let proof = core_and_leader_proofs_generator
            .get_next_core_proof()
            .await
            .unwrap();
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

    // Verify any traces of leader proofs have been removed.
    assert!(
        core_and_leader_proofs_generator
            .leader_proofs_generator
            .is_none()
    );
    assert!(
        core_and_leader_proofs_generator
            .get_next_leader_proof()
            .await
            .is_none()
    );
    // Generate and verify last proof.
    let proof = core_and_leader_proofs_generator
        .get_next_core_proof()
        .await
        .unwrap();
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

    // Next proof should be `None` since we ran out of core quota.
    assert!(
        core_and_leader_proofs_generator
            .get_next_core_proof()
            .await
            .is_none()
    );
}

#[test(tokio::test)]
async fn epoch_private_info() {
    let core_quota = 10;
    let leadership_quota = 15;
    let (core_public_inputs, core_private_inputs) = valid_proof_of_quota_inputs(core_quota);
    let (leadership_public_inputs, leadership_private_inputs) =
        valid_proof_of_leader_inputs(leadership_quota);

    let mut core_and_leader_proofs_generator = RealCoreAndLeaderProofsGenerator::new(
        ProofsGeneratorSettings {
            local_node_index: None,
            membership_size: 1,
            public_inputs: leadership_public_inputs,
            encapsulation_layers: 1.try_into().unwrap(),
            epoch: Epoch::new(0),
        },
        CorePoQGeneratorFromPrivateCoreQuotaInputs::new(core_private_inputs.clone()),
    );

    core_and_leader_proofs_generator.set_epoch_private(
        leadership_private_inputs,
        leadership_public_inputs.leader,
        Epoch::new(1),
    );

    // Leadership proof should be generated and verified correctly.
    let proof = core_and_leader_proofs_generator
        .get_next_leader_proof()
        .await
        .unwrap();
    let verified_proof_of_quota = proof
        .proof_of_quota
        .into_inner()
        .verify(
            &poq_public_inputs_from_session_public_inputs_and_signing_key((
                leadership_public_inputs,
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

    // New proof should verify successfully.
    let proof = core_and_leader_proofs_generator
        .get_next_leader_proof()
        .await
        .unwrap();
    let verified_proof_of_quota = proof
        .proof_of_quota
        .into_inner()
        .verify(
            &poq_public_inputs_from_session_public_inputs_and_signing_key((
                leadership_public_inputs,
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

    // We override all the settings since we fixtures for core and leadership proofs
    // use a different set of public inputs.
    core_and_leader_proofs_generator.override_settings(ProofsGeneratorSettings {
        local_node_index: None,
        membership_size: 1,
        public_inputs: core_public_inputs,
        encapsulation_layers: 1.try_into().unwrap(),
        epoch: Epoch::new(0),
    });
    core_and_leader_proofs_generator.rotate_epoch(core_public_inputs.leader, Epoch::new(1));

    // We test that core proof generation still works fine
    let proof = core_and_leader_proofs_generator
        .get_next_core_proof()
        .await
        .unwrap();
    let verified_proof_of_quota = proof
        .proof_of_quota
        .into_inner()
        .verify(
            &poq_public_inputs_from_session_public_inputs_and_signing_key((
                core_public_inputs,
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

/// `rotate_epoch` with the same epoch as the current one should be a no-op:
/// the leader generator (if present) should be preserved.
#[test(tokio::test)]
async fn rotate_epoch_equal_is_noop() {
    let core_quota = 5;
    let leadership_quota = 10;
    let (_, core_private_inputs) = valid_proof_of_quota_inputs(core_quota);
    let (leadership_public_inputs, leadership_private_inputs) =
        valid_proof_of_leader_inputs(leadership_quota);

    let mut generator = RealCoreAndLeaderProofsGenerator::new(
        ProofsGeneratorSettings {
            local_node_index: None,
            membership_size: 1,
            public_inputs: leadership_public_inputs,
            encapsulation_layers: 1.try_into().unwrap(),
            epoch: Epoch::new(1),
        },
        CorePoQGeneratorFromPrivateCoreQuotaInputs::new(core_private_inputs),
    );

    // Provide private info so a leader generator exists.
    generator.set_epoch_private(
        leadership_private_inputs,
        leadership_public_inputs.leader,
        Epoch::new(1),
    );
    assert!(generator.leader_proofs_generator.is_some());

    // Rotating with the same epoch should be a no-op - leader generator
    // preserved.
    generator.rotate_epoch(leadership_public_inputs.leader, Epoch::new(1));
    assert!(
        generator.leader_proofs_generator.is_some(),
        "Leader generator should be preserved when rotating to the same epoch"
    );

    // Leader proofs should still work.
    let proof = generator.get_next_leader_proof().await;
    assert!(proof.is_some());
}

/// `rotate_epoch` drops the leader generator. `set_epoch_private` then
/// recreates it for the new epoch.
#[test(tokio::test)]
async fn rotate_epoch_drops_leader_then_set_epoch_private_recreates() {
    let core_quota = 5;
    let leadership_quota = 10;
    let (_, core_private_inputs) = valid_proof_of_quota_inputs(core_quota);
    let (leadership_public_inputs, leadership_private_inputs) =
        valid_proof_of_leader_inputs(leadership_quota);

    let mut generator = RealCoreAndLeaderProofsGenerator::new(
        ProofsGeneratorSettings {
            local_node_index: None,
            membership_size: 1,
            public_inputs: leadership_public_inputs,
            encapsulation_layers: 1.try_into().unwrap(),
            epoch: Epoch::new(0),
        },
        CorePoQGeneratorFromPrivateCoreQuotaInputs::new(core_private_inputs),
    );

    // Provide private info for epoch 0.
    generator.set_epoch_private(
        leadership_private_inputs.clone(),
        leadership_public_inputs.leader,
        Epoch::new(0),
    );
    assert!(generator.leader_proofs_generator.is_some());

    // Rotate to epoch 1 - leader generator should be dropped because its
    // epoch is behind.
    generator.rotate_epoch(leadership_public_inputs.leader, Epoch::new(1));
    assert!(
        generator.leader_proofs_generator.is_none(),
        "Leader generator should be dropped after rotating to a newer epoch"
    );
    assert!(generator.get_next_leader_proof().await.is_none());

    // Provide private info for epoch 1 - leader generator should be
    // recreated.
    generator.set_epoch_private(
        leadership_private_inputs,
        leadership_public_inputs.leader,
        Epoch::new(1),
    );
    assert!(
        generator.leader_proofs_generator.is_some(),
        "Leader generator should be recreated after set_epoch_private"
    );
    let proof = generator.get_next_leader_proof().await;
    assert!(proof.is_some());
}

/// Two consecutive `rotate_epoch` calls without `set_epoch_private` in between:
/// the first drops the leader generator, the second is a no-op on the leader
/// side since there is nothing to drop.
#[test(tokio::test)]
async fn double_rotate_epoch_without_set_epoch_private() {
    let core_quota = 10;
    let (public_inputs, private_inputs) = valid_proof_of_quota_inputs(core_quota);

    let mut generator = RealCoreAndLeaderProofsGenerator::new(
        ProofsGeneratorSettings {
            local_node_index: None,
            membership_size: 1,
            public_inputs,
            encapsulation_layers: 1.try_into().unwrap(),
            epoch: Epoch::new(0),
        },
        CorePoQGeneratorFromPrivateCoreQuotaInputs::new(private_inputs),
    );

    // Consume some core proofs.
    for _ in 0u8..3 {
        assert!(generator.get_next_core_proof().await.is_some());
    }

    // First rotation: epoch 0 -> 1.
    generator.rotate_epoch(public_inputs.leader, Epoch::new(1));
    assert!(generator.leader_proofs_generator.is_none());

    // Second rotation: epoch 1 -> 2.
    generator.rotate_epoch(public_inputs.leader, Epoch::new(2));
    assert!(generator.leader_proofs_generator.is_none());

    // Core proofs should still work - remaining quota preserved across rotations.
    // We consumed 3 out of 10, so 7 remain.
    for _ in 0u8..7 {
        assert!(generator.get_next_core_proof().await.is_some());
    }
    assert!(generator.get_next_core_proof().await.is_none());
}
