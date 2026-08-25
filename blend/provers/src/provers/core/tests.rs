use std::sync::Arc;

use lb_blend_proofs::{
    quota::{KeyIndex, Quota},
    selection::inputs::VerifyInputs,
};
use lb_cryptarchia_engine::Epoch;
use rayon::ThreadPoolBuilder;
use test_log::test;

use crate::provers::{
    ProofsGeneratorSettings,
    core::{CoreProofsGenerator as _, RealCoreProofsGenerator},
    test_utils::{
        CorePoQGeneratorFromPrivateCoreQuotaInputs,
        poq_public_inputs_from_epoch_public_inputs_and_signing_key, valid_proof_of_quota_inputs,
    },
};

#[test(tokio::test)]
async fn proof_generation() {
    let core_quota = Quota::new::<10>();
    let (public_inputs, private_inputs) = valid_proof_of_quota_inputs(core_quota);

    let mut core_proofs_generator = RealCoreProofsGenerator::new(
        ProofsGeneratorSettings {
            local_node_index: None,
            membership_size: 1,
            public_inputs,
            encapsulation_layers: 1.try_into().unwrap(),
            epoch: Epoch::new(0),
            pow_mining_pool: Arc::new(ThreadPoolBuilder::new().build().unwrap()),
        },
        KeyIndex::ZERO,
        CorePoQGeneratorFromPrivateCoreQuotaInputs::new(private_inputs.clone()),
    );

    for _ in 0..core_quota.get() {
        let proof = core_proofs_generator.get_next_proof().await.unwrap();
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

    // Next proof should be `None` since we ran out of core quota.
    assert!(core_proofs_generator.get_next_proof().await.is_none());
}

/// How many key indices the run before the restart got through.
const SPENT: u64 = 4;

/// A resumed generator hands out only what the epoch's quota has left.
///
/// The key-index range is the sole bound on core proofs, so it has to account
/// for the indices a previous run already spent — otherwise resuming would
/// either hand back proofs the quota cannot cover, or re-mint nullifiers the
/// earlier run already put on the wire.
#[test(tokio::test)]
async fn resumed_generator_is_bounded_by_what_the_quota_has_left() {
    let core_quota = Quota::new::<10>();
    let (public_inputs, private_inputs) = valid_proof_of_quota_inputs(core_quota);

    let mut core_proofs_generator = RealCoreProofsGenerator::new(
        ProofsGeneratorSettings {
            local_node_index: None,
            membership_size: 1,
            public_inputs,
            encapsulation_layers: 1.try_into().unwrap(),
            epoch: Epoch::new(0),
            pow_mining_pool: Arc::new(ThreadPoolBuilder::new().build().unwrap()),
        },
        Quota::try_new(SPENT).unwrap(),
        CorePoQGeneratorFromPrivateCoreQuotaInputs::new(private_inputs),
    );

    let mut generated = 0u64;
    while core_proofs_generator.get_next_proof().await.is_some() {
        generated += 1;
        assert!(
            generated <= core_quota.get() - SPENT,
            "a resumed generator must not outlive the quota it inherited"
        );
    }
    assert_eq!(generated, core_quota.get() - SPENT);
}
