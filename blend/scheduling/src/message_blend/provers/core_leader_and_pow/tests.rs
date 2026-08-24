use std::sync::Arc;

use lb_blend_proofs::{
    quota::{KeyIndex, Quota},
    selection::inputs::VerifyInputs,
};
use lb_cryptarchia_engine::Epoch;
use rayon::ThreadPoolBuilder;
use test_log::test;

use crate::message_blend::provers::{
    ProofsGeneratorSettings,
    core_leader_and_pow::{
        CoreLeaderAndPowProofsGenerator as _, RealCoreLeaderAndPowProofsGenerator,
    },
    test_utils::{
        CorePoQGeneratorFromPrivateCoreQuotaInputs,
        poq_public_inputs_from_epoch_public_inputs_and_signing_key, valid_proof_of_quota_inputs,
        valid_proof_of_work_inputs,
    },
};

#[test(tokio::test)]
async fn pow_proof_generation() {
    // The `PoW` fixture and the core fixture do not share public inputs, so the
    // generator is built with the former and only its `PoW` branch is exercised
    // here. Core and leadership generation is covered by the wrapped generator's
    // own tests; `core_proofs_are_delegated` covers the delegation itself.
    let public_inputs = {
        let mut public_inputs = valid_proof_of_work_inputs(Quota::ONE);
        // The wrapped core generator starts proving as soon as it is built, and
        // the core fixture's private inputs do not satisfy these public ones. A
        // zero core quota leaves it with nothing to prove.
        public_inputs.core.quota = Quota::ZERO;
        public_inputs
    };
    let (_, core_private_inputs) = valid_proof_of_quota_inputs(Quota::ONE);

    let mut generator = RealCoreLeaderAndPowProofsGenerator::new(
        ProofsGeneratorSettings {
            local_node_index: None,
            membership_size: 1,
            public_inputs,
            encapsulation_layers: 1.try_into().unwrap(),
            epoch: Epoch::new(0),
            pow_mining_pool: Arc::new(ThreadPoolBuilder::new().build().unwrap()),
        },
        KeyIndex::ZERO,
        CorePoQGeneratorFromPrivateCoreQuotaInputs::new(core_private_inputs),
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
async fn core_proofs_are_delegated() {
    let core_quota = Quota::ONE;
    let (public_inputs, core_private_inputs) = {
        let (mut public_inputs, private_inputs) = valid_proof_of_quota_inputs(core_quota);
        // The wrapped `PoW` generator starts mining as soon as it is built, and
        // the core fixture's difficulty is far too hard to ever solve. A zero
        // quota switches the branch off, which is what this test wants anyway.
        public_inputs.pow.pow_quota = Quota::ZERO;
        (public_inputs, private_inputs)
    };

    let mut generator = RealCoreLeaderAndPowProofsGenerator::new(
        ProofsGeneratorSettings {
            local_node_index: None,
            membership_size: 1,
            public_inputs,
            encapsulation_layers: 1.try_into().unwrap(),
            epoch: Epoch::new(0),
            pow_mining_pool: Arc::new(ThreadPoolBuilder::new().build().unwrap()),
        },
        KeyIndex::ZERO,
        CorePoQGeneratorFromPrivateCoreQuotaInputs::new(core_private_inputs),
    );

    let proof = generator.get_next_core_proof().await.unwrap();
    proof
        .proof_of_quota
        .into_inner()
        .verify(&poq_public_inputs_from_epoch_public_inputs_and_signing_key(
            (public_inputs, proof.ephemeral_signing_key.public_key()),
        ))
        .unwrap();

    // The core quota is spent, and the wrapper does not silently draw on
    // another branch to hide that.
    assert!(generator.get_next_core_proof().await.is_none());
    // Leadership proofs need private epoch info that was never provided.
    assert!(generator.get_next_leader_proof().await.is_none());
}
