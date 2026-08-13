use std::collections::HashSet;

use lb_blend_proofs::{
    quota::{Quota, inputs::prove::public::PowInputs},
    selection::inputs::VerifyInputs,
};
use lb_cryptarchia_engine::Epoch;
use lb_groth16::{AdditiveGroup as _, Field as _, Fr};
use test_log::test;

use crate::message_blend::provers::{
    ProofsGeneratorSettings,
    pow::{PowProofsGenerator as _, RealPowProofsGenerator},
    test_utils::{
        poq_public_inputs_from_epoch_public_inputs_and_signing_key, valid_proof_of_work_inputs,
    },
};

const ENCAPSULATION_LAYERS: u64 = 3;

/// Settings whose `PoW` public inputs are the fixture's, except for those
/// given here.
fn settings(pow_overrides: Option<PowInputs>) -> ProofsGeneratorSettings {
    let mut public_inputs = valid_proof_of_work_inputs(Quota::new::<ENCAPSULATION_LAYERS>());
    if let Some(pow_overrides) = pow_overrides {
        public_inputs.pow = pow_overrides;
    }

    ProofsGeneratorSettings {
        local_node_index: None,
        membership_size: 1,
        public_inputs,
        encapsulation_layers: ENCAPSULATION_LAYERS.try_into().unwrap(),
        epoch: Epoch::new(0),
    }
}

#[test(tokio::test)]
async fn proof_generation() {
    let settings = settings(None);
    let mut pow_proofs_generator = RealPowProofsGenerator::new(settings);

    // Two messages' worth of proofs, so that the second message is proved from
    // a solution mined after the first one's quota ran out.
    let mut key_nullifiers = HashSet::new();
    for _ in 0..2 * ENCAPSULATION_LAYERS {
        let proof = pow_proofs_generator.get_next_proof().await.unwrap();
        let verified_proof_of_quota = proof
            .proof_of_quota
            .into_inner()
            .verify(&poq_public_inputs_from_epoch_public_inputs_and_signing_key(
                (
                    settings.public_inputs,
                    proof.ephemeral_signing_key.public_key(),
                ),
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

        // A nullifier may be used only once, whether the proof comes from the
        // same solution at another index or from a different solution.
        assert!(key_nullifiers.insert(verified_proof_of_quota.key_nullifier()));
    }
}

#[test(tokio::test)]
async fn no_proof_when_the_puzzle_has_no_solution() {
    let mut pow_proofs_generator = RealPowProofsGenerator::new(settings(Some(PowInputs {
        pow_blend_difficulty: Fr::ZERO,
        pow_quota: Quota::new::<ENCAPSULATION_LAYERS>(),
    })));

    assert!(pow_proofs_generator.get_next_proof().await.is_none());
}

#[test(tokio::test)]
async fn no_proof_when_a_solution_cannot_cover_a_whole_message() {
    let mut pow_proofs_generator = RealPowProofsGenerator::new(settings(Some(PowInputs {
        // The largest field element: every ticket is a solution.
        pow_blend_difficulty: -Fr::ONE,
        // One key short of the encapsulations a single message needs.
        pow_quota: Quota::new::<{ ENCAPSULATION_LAYERS - 1 }>(),
    })));

    assert!(pow_proofs_generator.get_next_proof().await.is_none());
}
