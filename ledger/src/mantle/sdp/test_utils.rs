//! Test-only helpers shared between `mantle::sdp::tests` and the outer
//! [`crate::LedgerState`] tests in `lib.rs`.

use lb_blend_crypto::merkle::MerkleTree;
use lb_blend_message::reward::{BlendingToken, BlendingTokenEvaluation};
use lb_blend_proofs::{
    quota::{
        VerifiedProofOfQuota,
        inputs::prove::{
            PrivateInputs, PublicInputs,
            private::ProofOfCoreQuotaInputs,
            public::{CoreInputs, LeaderInputs},
        },
    },
    selection::VerifiedProofOfSelection,
};
use lb_core::{blend::core_quota, sdp::blend::ActivityProof};
use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey};

use crate::{EpochState, mantle::sdp::rewards::blend::RewardsParameters};

const SINGLE_NODE_SNAPSHOT_SIZE: usize = 1;

/// Generates an activity proof with a real `PoQ` + `PoSel` that the
/// `rewards` module will accept for the single-provider snapshot used
/// in these tests.
///
/// Grinds `message_release_index` until the resulting blending token's
/// Hamming distance falls within the activity threshold.
//
// TODO: Remove this after making `SdpLedger` generic over `Rewards`,
//       which requires extensive changes across multiple crates.
pub fn generate_activity_proof(
    provider_zk_key: &ZkKey,
    target_epoch_state: &EpochState,
    current_epoch_state: &EpochState,
    blend_params: &RewardsParameters,
) -> ActivityProof {
    let activity_epoch = target_epoch_state.epoch();
    let zk_id = provider_zk_key.to_public_key();
    // Build a Merkle tree with a single leaf corresponding to the provider's zk_id.
    // This function works only for single-provider snapshots.
    let merkle_tree = MerkleTree::new(vec![zk_id.into_inner()]).unwrap();
    let core_path_and_selectors = merkle_tree.get_proof_for_key(&zk_id.into_inner()).unwrap();

    let quota = core_quota(
        blend_params.rounds_per_epoch,
        blend_params.message_frequency_per_round,
        blend_params.num_blend_layers,
        SINGLE_NODE_SNAPSHOT_SIZE,
    );
    let num_blend_layers = blend_params.num_blend_layers.get();
    let message_quota =
        num_blend_layers + (num_blend_layers * blend_params.data_replication_factor);
    // Match `RewardsParameters::leader_inputs(target_epoch_state)`: the target
    // snapshot's `proof_verifier` was constructed with these values, so any
    // proof must reproduce them exactly.
    let leader_inputs = LeaderInputs {
        pol_ledger_aged: target_epoch_state.utxos.root(),
        pol_epoch_nonce: *target_epoch_state.nonce(),
        message_quota,
        lottery_0: target_epoch_state.lottery_0,
        lottery_1: target_epoch_state.lottery_1,
    };
    let core_inputs = CoreInputs {
        zk_root: merkle_tree.root(),
        quota,
    };
    let token_evaluation = BlendingTokenEvaluation::new(
        quota,
        SINGLE_NODE_SNAPSHOT_SIZE as u64,
        blend_params.activity_threshold_sensitivity,
    )
    .unwrap();
    // `current_epoch_state.nonce()` equals `current_epoch_state.epoch_randomness()`
    // in the rewards module.
    let epoch_randomness = (*current_epoch_state.nonce()).into();

    // Ephemeral signing key (separate from the provider's identity key —
    // matches production behaviour where proofs use ephemeral keys).
    let ephemeral = Ed25519Key::from_bytes(&[7u8; 32]);
    let public_inputs = PublicInputs {
        signing_key: ephemeral.public_key().into_inner(),
        core: core_inputs,
        leader: leader_inputs,
    };
    for message_release_index in 0u64.. {
        let private_inputs = PrivateInputs::new_proof_of_core_quota_inputs(
            message_release_index,
            ProofOfCoreQuotaInputs {
                core_path_and_selectors,
                core_sk: *provider_zk_key.as_fr(),
            },
        );
        let Ok((poq, secret_selection_randomness)) =
            VerifiedProofOfQuota::new(&public_inputs, private_inputs)
        else {
            continue;
        };
        let posel = VerifiedProofOfSelection::new(secret_selection_randomness);
        let token = BlendingToken::new(ephemeral.public_key(), poq, posel);
        if token_evaluation
            .evaluate(&token, epoch_randomness)
            .is_some()
        {
            return ActivityProof {
                epoch: activity_epoch,
                signing_key: ephemeral.public_key(),
                proof_of_quota: poq.into(),
                proof_of_selection: posel.into(),
            };
        }
    }
    unreachable!("failed to grind message_release_index")
}
