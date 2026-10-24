//! The zone stories on the simulated node, each run on every chain of the
//! catalogue.

use std::collections::{HashMap, HashSet};

use common::{
    FOREIGN, FOREIGN_2, Posted, World, assert_exactly_once_any_order, filtered, inscription,
    passive, reference, seq_key, strs,
};
use lb_core::{
    crypto::Hash,
    mantle::{
        Note, NoteId, Op, OpProof, SignedOps, Utxo,
        ledger::{Inputs, Outputs},
        ops::{
            OpId as _,
            channel::{MsgId, VerifiedChannelKeys, deposit::DepositOp, inscribe::Inscription},
            op_proof::samples::SampleProof as _,
            transfer::TransferOp,
        },
        traits::Hashable as _,
        transactions::{GasPrices, Ops, tx_list::OpProofs},
    },
    proofs::channel_multi_sig_proof::IndexedSignatures,
};
use lb_key_management_system_service::keys::ZkSignature;
use lb_zone_sdk::sequencer::{
    Error as SdkError, FinalizedOp, PendingTx, PreparedAtomicBundle, PreparedChannelConfig,
    SequencerConfig, WithdrawArg, WithdrawInputs,
};
use logos_blockchain_zone_sdk_testing::{
    chains::{self, Chain, Shape},
    consumer::ReferenceConsumer,
    for_each_base_condition, for_each_chain,
    harness::RECONNECT_DELAY,
    layers::Layer,
    node::{BlockSpec, PENDING_NOTE_EXPIRY_BLOCKS, TxSelection},
    policy::payload_str,
    txs, world as common,
    writers::WriterOp,
};

/// Run `$body` once per catalogue chain.
macro_rules! on_chains {
    ($name:ident, $body:ident; $($chain:ident),* $(,)?) => {
        mod $name {
            use super::*;
            $(
                #[tokio::test(start_paused = true)]
                async fn $chain() {
                    $body(chains::$chain()).await;
                }
            )*
        }
    };
}

macro_rules! scenario {
    ($name:ident, $body:ident) => {
        for_each_chain!(on_chains, $name, $body);
    };
}

/// The second tier: a core story on every ordered pair of base conditions,
/// one test per pair, named `<story>::<first>::<second>`.
macro_rules! on_pair_seconds {
    ($body:ident, $first:ident; $($second:ident),* $(,)?) => {
        mod $first {
            use super::*;
            $(
                #[tokio::test(start_paused = true)]
                async fn $second() {
                    $body(chains::pair(stringify!($first), stringify!($second))).await;
                }
            )*
        }
    };
}

macro_rules! on_pair_firsts {
    ($body:ident; $($first:ident),* $(,)?) => {
        $( for_each_base_condition!(on_pair_seconds, $body, $first); )*
    };
}

macro_rules! pairs {
    ($name:ident, $body:ident) => {
        mod $name {
            use super::*;
            for_each_base_condition!(on_pair_firsts, $body);
        }
    };
}

mod bundles;
mod configs;
mod known_issues;
mod notes;
mod publishing;
mod tiers;

fn keys(names: &[&str]) -> VerifiedChannelKeys {
    VerifiedChannelKeys::try_from(
        names
            .iter()
            .map(|n| seq_key(n).public_key())
            .collect::<Vec<_>>(),
    )
    .expect("at least one key")
}

fn sign_config(prepared: &PreparedChannelConfig, signers: &[&str]) -> IndexedSignatures {
    let mut signatures = IndexedSignatures::empty();
    for name in signers {
        let signature = prepared
            .sign_with(&seq_key(name))
            .expect("accredited signer");
        signatures
            .try_insert(signature.channel_key_index, signature.signature)
            .expect("one signature per key");
    }
    signatures
}

/// Prepare a config on `preparer`, sign with `signers`, submit from
/// `submitter` and step until it is finalized.
async fn escalate_config(
    world: &mut World,
    preparer: usize,
    submitter: usize,
    names: &[&str],
    threshold: u16,
    timeframe: u32,
    signers: &[&str],
) {
    let config_keys = keys(names);
    let prepared = world
        .harness
        .seq(preparer)
        .handle()
        .prepare_channel_config(
            config_keys,
            timeframe.into(),
            0.into(),
            threshold,
            threshold,
        )
        .await
        .expect("prepared config");
    let proposed = prepared.proposed_config();
    assert_eq!(proposed.configuration_threshold, threshold);
    assert_eq!(proposed.transfer_threshold, threshold);
    assert_eq!(proposed.keys.len(), names.len());
    let signatures = sign_config(&prepared, signers);
    let (result, _) = world
        .harness
        .seq(submitter)
        .handle()
        .submit_channel_config(prepared, signatures)
        .expect("config submitted");
    world.harness.settle().await;
    let hash = result.inscription_id();
    world.include(hash).await;
    world.finalize_tx(hash).await;
}

fn pin_payload(op_id: &Hash) -> Inscription {
    inscription(&format!("pin deposit {op_id:?}"))
}

fn withdraw_payload(op_id: &Hash) -> Inscription {
    inscription(&format!("withdraw deposit {op_id:?}"))
}

fn count_payload(payloads: &[String], payload: &str) -> usize {
    payloads.iter().filter(|p| p.as_str() == payload).count()
}

/// Sign `prepared` with `signers` and submit it from `index`; the bundle's
/// tx hash.
async fn submit_bundle(
    world: &mut World,
    index: usize,
    prepared: PreparedAtomicBundle,
    signers: &[&str],
) -> lb_zone_sdk::node_types::TxHash {
    try_submit_bundle(world, index, prepared, signers)
        .await
        .expect("bundle submitted")
}

/// Sign `prepared` with `signers` and submit it from `index`, as the SDK
/// takes it.
async fn try_submit_bundle(
    world: &mut World,
    index: usize,
    prepared: PreparedAtomicBundle,
    signers: &[&str],
) -> Result<lb_zone_sdk::node_types::TxHash, SdkError> {
    let mut signatures = IndexedSignatures::empty();
    for name in signers {
        let signature = prepared
            .sign_with(&seq_key(name))
            .expect("accredited signer");
        signatures
            .try_insert(signature.channel_key_index, signature.signature)
            .expect("one signature per key");
    }
    let result = world
        .harness
        .seq(index)
        .handle()
        .submit_atomic_bundle(prepared, signatures)
        .map(|(result, _)| result.inscription_id());
    world.harness.settle().await;
    result
}

/// Step until a running sequencer's wallet holds every note of `ids`, and
/// return its index: the one that can spend them now. With `only`, that
/// sequencer's.
async fn wait_wallet_holds(world: &mut World, ids: &[NoteId], only: Option<usize>) -> usize {
    let holder = |w: &mut World| {
        (0..w.harness.seqs.len()).find(|&i| {
            only.is_none_or(|only| only == i) && w.harness.seqs[i].is_running() && {
                let wallet = w.wallet(i);
                ids.iter().all(|id| {
                    wallet
                        .finalized
                        .iter()
                        .chain(wallet.unfinalized.iter())
                        .any(|n| n.note_id == *id)
                })
            }
        })
    };
    let found = world.step_until(40, |w| holder(w).is_some()).await;
    assert!(found, "no wallet holds the notes:\n{}", world.diagnostics());
    holder(world).expect("holder")
}

fn not_our_turn_with_pending(world: &mut World, index: usize, pending: usize) -> bool {
    let view = world.view(index);
    view.own_key_index == Some(1)
        && view.authorized_key_index.is_some()
        && !view.our_turn_to_write
        && view.authorized_key_index != view.own_key_index
        && view.pending_publish_txs == pending
}

async fn round_robin_queue(chain: Chain, depth: usize, payloads: [&str; 3], unlimited: bool) {
    let mut world = World::new(chain);
    let config = world.config_with_depth(depth);
    let a = world.start_with("SEQ_A", config.clone(), passive()).await;
    world.config_finalized(a, &["SEQ_B"], 2, 0).await;
    let b = world.start_with("SEQ_B", config, passive()).await;

    assert!(
        world
            .step_until(40, |w| not_our_turn_with_pending(w, b, 0))
            .await,
        "SEQ_B never reached the idle off-turn state"
    );
    let mut hashes = Vec::new();
    for payload in payloads {
        hashes.push(
            world
                .publish(b, payload)
                .await
                .expect("queued")
                .inscription_id(),
        );
    }
    assert!(
        world
            .step_until(40, |w| not_our_turn_with_pending(w, b, 3))
            .await,
        "SEQ_B did not hold 3 pending"
    );
    let checkpoint = world.checkpoint(b);
    world.stop(b);
    world.restart(b, Some(checkpoint)).await;
    assert!(
        world
            .step_until(40, |w| not_our_turn_with_pending(w, b, 3))
            .await,
        "pending outbox not restored from the checkpoint"
    );

    world.wait_turn(b).await;
    // The depth bounds what is in flight unmined: with depth two the third
    // message is posted only once one of the first two has landed, so it
    // cannot be on chain in the block that lands the first; unlimited, all
    // three go at once.
    world.include(hashes[0]).await;
    if !unlimited {
        assert!(
            !world.node().is_included(&hashes[2]),
            "the third message landed before the window drained"
        );
    }
    world.include(hashes[1]).await;
    world.include(hashes[2]).await;
    assert!(
        world.step_until(60, |w| w.pending_count(b) == 0).await,
        "pending never drained"
    );
    let payloads_on_chain = world.indexer_payloads().await;
    assert_exactly_once_any_order(&payloads_on_chain, &payloads);
    world.finish();
}

fn wallet_values(view: &lb_zone_sdk::sequencer::ChannelWalletView) -> (Vec<u64>, Vec<u64>) {
    (
        view.finalized.iter().map(|n| n.value).collect(),
        view.unfinalized.iter().map(|n| n.value).collect(),
    )
}

fn finalized_deposit<'a>(
    finalized: &'a [lb_zone_sdk::sequencer::FinalizedTx],
    op: &DepositOp,
    amount: u64,
) -> Option<&'a lb_zone_sdk::sequencer::DepositInfo> {
    finalized
        .iter()
        .flat_map(|tx| tx.ops.iter())
        .find_map(|f| match f {
            FinalizedOp::Deposit(d)
                if d.inputs == op.inputs && d.amount == amount && d.metadata == op.metadata =>
            {
                Some(d)
            }
            _ => None,
        })
}

/// Fee margin of the cucumber atomic deposit step; the mandatory fee varies
/// with input count and change.
const ATOMIC_DEPOSIT_FEE_MARGIN: u64 = 10_000;
const ATOMIC_DEPOSIT_FUNDING: u64 = 20_000;

fn finalized_transfer_input_counts(
    finalized: &[lb_zone_sdk::sequencer::FinalizedTx],
) -> Vec<usize> {
    finalized
        .iter()
        .flat_map(|tx| tx.ops.iter())
        .filter_map(|f| match f {
            FinalizedOp::ChannelTransfer(t) => Some(t.op.inputs.len()),
            _ => None,
        })
        .collect()
}

fn fee_notes(
    checkpoint: &lb_zone_sdk::sequencer::SequencerCheckpoint,
    hash: &lb_zone_sdk::node_types::TxHash,
) -> Vec<NoteId> {
    checkpoint
        .pending_txs
        .iter()
        .filter(|(h, _)| h == hash)
        .flat_map(|(_, tx)| {
            tx.op_refs_iter()
                .filter_map(|op| match op {
                    lb_core::mantle::OpRef::Transfer(t) => {
                        Some(t.inputs.iter().copied().collect::<Vec<_>>())
                    }
                    _ => None,
                })
                .flatten()
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Slot of the block that included `hash`.
fn included_slot(world: &World, hash: &lb_zone_sdk::node_types::TxHash) -> u64 {
    let node = world.node();
    node.block(node.block_of_tx(hash).expect("included"))
        .expect("block")
        .slot
        .into_inner()
}
