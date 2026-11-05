//! Proof for finding `20261023T054633-3d44ae/1` (spec domain).
//!
//! Bedrock v1.1 Mantle specification, "Proof of Work Operations",
//! `CLAIM_POW_REWARD`:
//!
//! * Proof: "A `ZkSignature` by the secret key corresponding to `public_key`,
//!   over the transaction's `mantle_txhash`. The signature proves knowledge of
//!   that secret key, so a solution cannot be found by searching over public
//!   keys directly."
//! * Validation rule 6, "The claim must be signed by the key the reward is
//!   paid to": `assert ZkSignature_verify(mantle_txhash, claim_proof,
//!   [claim.public_key])`, where `claim_proof` is "the `op_proofs` entry for
//!   this Operation".
//! * Example: `op_proofs=[ZkSignature(reward_sk, mantle_txhash(tx)),
//!   transfer.prove(reward_sk)]`.
//!
//! Both tests admit a transaction through the public API on the path a block's
//! transactions take: `SignedOps::from_parts` (pairs each op with its proof),
//! `preverify`, `LedgerState::try_apply_transaction` (stateful validation and
//! execution) and the deferred ZK proof batch. Each fails on the code as it
//! stands and passes once the claim carries, and is validated against, a
//! `ZkSignature` by `public_key` over `mantle_txhash`.
#![allow(non_snake_case, reason = "the test target is named after the finding id")]

use std::{
    collections::HashMap,
    num::{NonZero, NonZeroU32, NonZeroU64},
    sync::Arc,
};

use lb_core::{
    mantle::{
        Note, Op, OpProof, SignedOps, Utxo, Value,
        gas::MainnetGasProfile,
        ledger::{Inputs, Outputs, verification_mode::StandardMode},
        ops::{NoOpProof, OpId as _, pow::ClaimPowRewardOp, transfer::TransferOp},
        traits::Hashable as _,
        transactions::{OpProofs, Ops, states::Unverified},
    },
    sdp::{MinStake, ServiceParameters, ServiceType},
};
use lb_cryptarchia_engine::EpochConfig;
use lb_groth16::Fr;
use lb_key_management_system_keys::keys::{ZkKey, ZkPublicKey};
use lb_utils::math::{NonNegativeRatio, PositiveF64};
use logos_blockchain_ledger::{
    Config, Ledger, LedgerState,
    config::{BlendPoWConfig, ModulusShift, PoWConfig, RewardPoWConfig},
    mantle::sdp::{self, ServiceRewardsParameters, rewards::blend::RewardsParameters},
};

/// Id of the ledger's root block: a recent canonical block, recorded among the
/// seen blocks at slot 0, that claims anchor to.
const ROOT: [u8; 32] = [0xB0; 32];
/// `epoch_pow_reward` at genesis.
const EPOCH_POW_REWARD: Value = 1_000_000;
/// Value of a genesis note, owned by [`fee_key`], that pays fees.
const FEE_NOTE_VALUE: Value = 1_000_000;
/// What each transaction leaves for fees: far above its gas cost at the
/// genesis prices of 1 per gas, even with a signature-verifying claim.
const FEE: Value = 100_000;

/// The test ledger configuration (the in-crate test schedule: `k = 1`,
/// `f = 1/10`), with `PoW` reward claiming enabled. The genesis reward target
/// is `p / 2`, so about half of all tickets satisfy it and a winning key is
/// found within a few tries.
fn config() -> Config {
    let mut service_params = HashMap::new();
    service_params.insert(
        ServiceType::BlendNetwork,
        ServiceParameters {
            inactivity_period: 2.try_into().unwrap(),
            epoch: 0.into(),
        },
    );
    let epoch_config = EpochConfig {
        epoch_stake_distribution_stabilization: NonZero::new(3).unwrap(),
        epoch_period_nonce_buffer: NonZero::new(3).unwrap(),
        epoch_period_nonce_stabilization: NonZero::new(4).unwrap(),
    };
    let consensus_config = lb_cryptarchia_engine::Config::new(
        NonZero::new(1).unwrap(),
        NonNegativeRatio::new(1, 10.try_into().unwrap()),
        1f64.try_into().expect("1 > 0"),
        NonZero::new(12).unwrap(),
    );
    let epoch_length = epoch_config.epoch_length(consensus_config.base_period_length());

    Config {
        epoch_config,
        consensus_config,
        sdp_config: sdp::Config {
            service_params: Arc::new(service_params),
            service_rewards_params: ServiceRewardsParameters {
                blend: RewardsParameters {
                    rounds_per_epoch: epoch_length.try_into().unwrap(),
                    message_frequency_per_round: PositiveF64::try_from(1.0).unwrap(),
                    num_blend_layers: NonZeroU64::new(3).unwrap(),
                    minimum_network_size: NonZeroU64::new(1).unwrap(),
                    data_replication_factor: 0,
                    activity_threshold_sensitivity: 1,
                },
            },
            min_stake: MinStake {
                threshold: 1,
                timestamp: 0,
            },
        },
        faucet_pk: None,
        pow_config: PoWConfig {
            blend: BlendPoWConfig {
                base_difficulty: ModulusShift::new::<234>(),
                target_transactions_per_block: NonZeroU64::new(10).unwrap(),
                max_step: NonZeroU64::new(4).unwrap(),
                damping_num: NonZeroU32::new(1).unwrap(),
                damping_den_offset: 1,
            },
            reward: RewardPoWConfig {
                reward_pool_genesis: 1_000_000_000,
                epoch_reward_genesis: EPOCH_POW_REWARD,
                minimum_difficulty: ModulusShift::new::<1>(),
                ema_smoothing_factor: 9,
                ema_smoothing_precision: NonZeroU64::new(10).unwrap(),
                target_claims_per_block: 1,
                rate_num: 1,
                rate_den: NonZeroU64::new(10).unwrap(),
                target_claim_per_block: NonZeroU64::MIN,
                pow_share: 10,
                share_den: NonZeroU64::new(100).unwrap(),
                slot_window: NonZeroU64::new(300).unwrap(),
            },
        },
    }
}

fn fee_key() -> ZkKey {
    ZkKey::new(Fr::from(7u64))
}

/// The state of the root block, `ROOT`, of a ledger whose only note is the fee
/// note. `Ledger::new` records `ROOT` among the seen blocks claims may anchor
/// to, at the root's slot, which is also the current slot of this state.
fn root_state(config: &Config) -> (LedgerState, Utxo) {
    let fee_note = Utxo::new(
        [0xFE; 32],
        0,
        Note::new(FEE_NOTE_VALUE, fee_key().to_public_key()),
    );
    let ledger = Ledger::new(
        ROOT,
        LedgerState::from_utxos([fee_note], config),
        config.clone(),
    );
    let state = ledger.state(&ROOT).expect("the root state exists").clone();
    (state, fee_note)
}

/// A claim anchored to `ROOT`, against the current epoch nonce, paying
/// `public_key`.
fn claim_for(state: &LedgerState, public_key: ZkPublicKey) -> ClaimPowRewardOp {
    ClaimPowRewardOp {
        epoch_nonce: state.epoch_state().nonce,
        block_hash: ROOT,
        public_key,
    }
}

/// Validation rule 4: the ticket satisfies the reward threshold.
fn satisfies_reward_threshold(state: &LedgerState, claim: &ClaimPowRewardOp) -> bool {
    Fr::from(claim.get_puzzle_ticket()) < state.mantle_ledger().pow.reward_difficulty()
}

/// Admit a transaction the way a block's transactions are admitted, returning
/// the state after it, or the stage that rejected it.
fn admit(
    config: &Config,
    state: &LedgerState,
    ops: Ops,
    op_proofs: OpProofs,
) -> Result<LedgerState, String> {
    let tx = SignedOps::<Unverified, StandardMode>::from_parts(ops, op_proofs)
        .map_err(|error| format!("SignedOps::from_parts: {error}"))?
        .preverify()
        .map_err(|error| format!("preverify: {error}"))?;
    let (after, _gas_and_fees, _events, deferred_zkps) = state
        .clone()
        .try_apply_transaction::<_, [u8; 32], MainnetGasProfile>(config, &tx)
        .map_err(|error| format!("try_apply_transaction: {error}"))?;
    deferred_zkps
        .verify()
        .map_err(|error| format!("deferred ZK proofs: {error:?}"))?;
    Ok(after)
}

/// Rule 6 and the Proof: the claim's `op_proofs` entry must be a `ZkSignature`
/// by `public_key` over `mantle_txhash`. This claim has none — its `public_key`
/// is a field element found by searching over public keys directly, with no
/// secret key behind it — so the transaction must be rejected.
#[test]
fn claim_without_a_zk_signature_by_its_public_key_is_rejected() {
    let config = config();
    let (state, fee_note) = root_state(&config);
    let pow_before = &state.mantle_ledger().pow;
    let (pool_before, epoch_pow_reward) = (pow_before.reward_pool(), pow_before.epoch_reward());

    let claim = (1u64..)
        .map(|candidate| claim_for(&state, ZkPublicKey::new(Fr::from(candidate))))
        .find(|claim| satisfies_reward_threshold(&state, claim))
        .expect("about half of all tickets satisfy p / 2");

    // The fee is paid from an unrelated note, so the claim's own `op_proofs`
    // entry is the only thing that could tie the claim to `public_key`.
    let fee_key = fee_key();
    let fee_transfer = TransferOp::new(
        Inputs::try_new(vec![fee_note.id()]).unwrap(),
        Outputs::try_new(vec![Note::new(
            FEE_NOTE_VALUE - FEE,
            fee_key.to_public_key(),
        )])
        .unwrap(),
    );
    let ops = Ops::from([Op::ClaimPowReward(claim.clone()), Op::Transfer(fee_transfer)]);
    let mantle_txhash = ops.hash().to_fr();
    let op_proofs = OpProofs::from([
        OpProof::None(NoOpProof),
        OpProof::ZkSig(ZkKey::multi_sign(&[fee_key], &mantle_txhash).unwrap()),
    ]);

    let outcome = admit(&config, &state, ops, op_proofs);

    let reward_note = Utxo::new(
        claim.op_id(),
        0,
        Note::new(epoch_pow_reward, claim.public_key),
    );
    let (minted, pool_after) = outcome.as_ref().map_or((false, pool_before), |after| {
        (
            after.latest_utxos().contains(&reward_note.id()),
            after.mantle_ledger().pow.reward_pool(),
        )
    });
    assert!(
        outcome.is_err(),
        "CLAIM_POW_REWARD validation rule 6 is not enforced: a claim whose op_proofs entry is \
         no ZkSignature (OpProof::None), paying public_key {:?} (a field element searched for \
         directly, with no secret key), was accepted; reward note minted: {minted}; \
         pow_reward_pool {pool_before} -> {pool_after}",
        claim.public_key,
    );
}

/// The spec's worked example: the claim is signed by `reward_sk` over
/// `mantle_txhash`, and the reward note it creates pays the fee in the
/// following `TRANSFER`. The transaction must be accepted.
#[test]
fn spec_example_claim_signed_over_mantle_txhash_is_accepted() {
    let config = config();
    let (state, _fee_note) = root_state(&config);
    let epoch_pow_reward = state.mantle_ledger().pow.epoch_reward();

    // `reward_pk`: "a key whose ticket satisfies difficulty_reward", found by
    // searching over secret keys.
    let (reward_sk, claim) = (1_000u64..)
        .map(|candidate| {
            let reward_sk = ZkKey::new(Fr::from(candidate));
            let claim = claim_for(&state, reward_sk.to_public_key());
            (reward_sk, claim)
        })
        .find(|(_, claim)| satisfies_reward_threshold(&state, claim))
        .expect("about half of all tickets satisfy p / 2");
    let reward_pk = reward_sk.to_public_key();

    // reward_note_id = derive_note_id(derive_op_id(claim), 0,
    //                                 Note(value=epoch_pow_reward, public_key=reward_pk))
    let reward_note_id = Utxo::new(claim.op_id(), 0, Note::new(epoch_pow_reward, reward_pk)).id();
    // transfer = Transfer(inputs=[reward_note_id], outputs=[<change_note>])
    let change_note = Note::new(epoch_pow_reward - FEE, reward_pk);
    let transfer = TransferOp::new(
        Inputs::try_new(vec![reward_note_id]).unwrap(),
        Outputs::try_new(vec![change_note]).unwrap(),
    );
    let ops = Ops::from([Op::ClaimPowReward(claim), Op::Transfer(transfer.clone())]);
    let mantle_txhash = ops.hash().to_fr();
    // op_proofs=[ZkSignature(reward_sk, mantle_txhash(tx)), transfer.prove(reward_sk)]
    let reward_signature = || {
        OpProof::ZkSig(ZkKey::multi_sign(std::slice::from_ref(&reward_sk), &mantle_txhash).unwrap())
    };
    let op_proofs = OpProofs::from([reward_signature(), reward_signature()]);

    let outcome = admit(&config, &state, ops, op_proofs);

    assert!(
        outcome.is_ok(),
        "the spec's CLAIM_POW_REWARD example (claim op_proofs entry = ZkSignature(reward_sk, \
         mantle_txhash)) was rejected: {}",
        outcome.as_ref().err().map_or("", String::as_str),
    );
    let after = outcome.unwrap();
    let change_id = Utxo::new(transfer.op_id(), 0, change_note).id();
    assert!(
        after.latest_utxos().contains(&change_id) && !after.latest_utxos().contains(&reward_note_id),
        "the reward note should pay the fee in the following TRANSFER"
    );
}
