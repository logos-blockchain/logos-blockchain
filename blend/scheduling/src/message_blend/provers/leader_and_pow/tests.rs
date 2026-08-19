use futures::stream;
use lb_blend_proofs::{quota::Quota, selection::inputs::VerifyInputs};
use lb_cryptarchia_engine::Epoch;
use test_log::test;

use crate::message_blend::provers::{
    ProofsGeneratorSettings,
    leader_and_pow::{LeaderAndPowProofsGenerator as _, RealLeaderAndPowProofsGenerator},
    test_utils::{
        poq_public_inputs_from_epoch_public_inputs_and_signing_key, valid_proof_of_leader_inputs,
        valid_proof_of_work_inputs,
    },
};

#[test(tokio::test)]
async fn pow_proof_generation() {
    // The `PoW` fixture and the leadership fixture do not share public inputs,
    // so the generator is built with the former and only its `PoW` branch is
    // exercised here. Leadership generation is covered by the wrapped
    // generator's own tests.
    let public_inputs = valid_proof_of_work_inputs(Quota::ONE);

    let mut generator = RealLeaderAndPowProofsGenerator::new(
        ProofsGeneratorSettings {
            local_node_index: None,
            membership_size: 1,
            public_inputs,
            encapsulation_layers: 1.try_into().unwrap(),
            epoch: Epoch::new(0),
        },
        Box::pin(stream::empty()),
    );

    let proof = generator.get_next_pow_proof().await.unwrap();
    let verified_proof_of_quota = proof
        .proof_of_quota
        .into_inner()
        .verify(&poq_public_inputs_from_epoch_public_inputs_and_signing_key(
            (public_inputs, proof.ephemeral_signing_key.public_key()),
        ))
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

#[test(tokio::test)]
async fn leadership_proofs_are_delegated() {
    let (public_inputs, leadership_private_inputs) = {
        let (mut public_inputs, private_inputs) = valid_proof_of_leader_inputs(Quota::ONE);
        // The wrapped `PoW` generator starts mining as soon as it is built, and
        // the leadership fixture's difficulty is far too hard to ever solve. A
        // zero quota switches the branch off, which is what this test wants
        // anyway.
        public_inputs.pow.pow_quota = Quota::ZERO;
        (public_inputs, private_inputs)
    };

    let mut generator = RealLeaderAndPowProofsGenerator::new(
        ProofsGeneratorSettings {
            local_node_index: None,
            membership_size: 1,
            public_inputs,
            encapsulation_layers: 1.try_into().unwrap(),
            epoch: Epoch::new(0),
        },
        Box::pin(stream::repeat(leadership_private_inputs)),
    );

    let proof = generator.get_next_leader_proof().await.unwrap();
    proof
        .proof_of_quota
        .into_inner()
        .verify(&poq_public_inputs_from_epoch_public_inputs_and_signing_key(
            (public_inputs, proof.ephemeral_signing_key.public_key()),
        ))
        .unwrap();
}
