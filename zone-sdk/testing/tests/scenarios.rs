//! The zone cucumber scenarios on the simulated node, each run on every
//! network behaviour.

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use common::{
    FOREIGN, Network, World, assert_exactly_once_any_order, filtered, filtered_first_occurrence,
    inscription, passive, reference, seq_key, strs,
};
use drivers::{
    BalanceAwarePolicy, BundleAnnounce, CustomRepublishPolicy, DepositLifecyclePolicy,
    DepositWithdrawPolicy, MultiSigBus, MultiSigLifecyclePolicy, SortedConflictPolicy,
    balance_update_payload, parse_balance_payload, pin_payload, withdraw_payload,
};
use lb_core::{
    mantle::{
        Note, Op, OpProof, SignedOps, Utxo,
        ledger::{Inputs, Outputs},
        ops::{
            OpId as _,
            channel::{VerifiedChannelKeys, deposit::DepositOp},
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
    FinalizedOp, PendingTx, PreparedChannelConfig, SequencerConfig, WithdrawArg, WithdrawInputs,
};
use logos_blockchain_zone_sdk_testing::{
    consumer::ReferenceConsumer,
    harness::RECONNECT_DELAY,
    node::{BlockSpec, TxSelection},
    policy::payload_str,
    txs, world as common,
    writers::WriterOp,
};

#[path = "scenarios/drivers.rs"]
mod drivers;

/// Run `$body` once per [`Network`].
macro_rules! scenario {
    ($name:ident, $body:ident) => {
        mod $name {
            use super::*;

            #[tokio::test(start_paused = true)]
            async fn linear() {
                $body(Network::Linear).await;
            }

            #[tokio::test(start_paused = true)]
            async fn shallow_forks() {
                $body(Network::ShallowForks).await;
            }

            #[tokio::test(start_paused = true)]
            async fn deep_forks() {
                $body(Network::DeepForks).await;
            }

            #[tokio::test(start_paused = true)]
            async fn stream_drops() {
                $body(Network::StreamDrops).await;
            }

            #[tokio::test(start_paused = true)]
            async fn delayed_inclusion() {
                $body(Network::DelayedInclusion).await;
            }

            #[tokio::test(start_paused = true)]
            async fn partitions() {
                $body(Network::Partitions).await;
            }
        }
    };
}

fn keys(names: &[&str]) -> VerifiedChannelKeys {
    VerifiedChannelKeys::try_from(
        names
            .iter()
            .map(|n| seq_key(n).public_key())
            .collect::<Vec<_>>(),
    )
    .expect("at least one key")
}

/// S1: publish messages, read them back from the indexer, add a config.
async fn publish_and_index(network: Network) {
    let mut world = World::new(network);
    let a = world.start("SEQ_A", passive()).await;

    let mut hashes = Vec::new();
    for payload in ["Hello, Zone!", "Second message", "Third message"] {
        hashes.push(world.publish_included(a, payload).await);
    }
    let config_keys = keys(&["SEQ_A", "SEQ_B"]);
    let (config, _) = world
        .harness
        .seq(a)
        .handle()
        .channel_config(config_keys, 0.into(), 0.into(), 1, 1)
        .await
        .expect("config accepted");
    let config_hash = config.0.inscription_id();
    world.harness.settle().await;

    world.finalize_all(&hashes).await;
    world.include(config_hash).await;
    world.finalize_tx(config_hash).await;

    let payloads = world.indexer_payloads().await;
    let expected = ["Hello, Zone!", "Second message", "Third message"];
    assert_eq!(
        filtered_first_occurrence(&payloads, &expected),
        strs(&expected)
    );
    world.finish();
}
scenario!(s01_publish_and_index, publish_and_index);

/// S2: resume from a checkpoint and keep publishing.
async fn resume_from_checkpoint(network: Network) {
    let mut world = World::new(network);
    let a = world.start("SEQ_A", passive()).await;
    let mut hashes = vec![
        world.publish_included(a, "Message 1").await,
        world.publish_included(a, "Message 2").await,
    ];
    let checkpoint = world.checkpoint(a);
    world.restart(a, Some(checkpoint)).await;
    hashes.push(world.publish_included(a, "Message 3").await);
    hashes.push(world.publish_included(a, "Message 4").await);
    world.finalize_all(&hashes).await;

    let expected = ["Message 1", "Message 2", "Message 3", "Message 4"];
    let payloads = world.indexer_payloads().await;
    assert_eq!(
        filtered_first_occurrence(&payloads, &expected),
        strs(&expected)
    );
    world.finish();
}
scenario!(s02_resume_from_checkpoint, resume_from_checkpoint);

/// S3: a stale checkpoint must not re-post what already finalized.
async fn resume_from_stale_checkpoint(network: Network) {
    let mut world = World::new(network);
    let a = world.start("SEQ_A", passive()).await;
    let hashes = vec![
        world.publish_included(a, "msg-1").await,
        world.publish_included(a, "msg-2").await,
    ];
    let stale = world.checkpoint(a);
    world.finalize_all(&hashes).await;

    world.restart(a, None).await;
    world.steps(2).await;
    let hashes = vec![
        world.publish_included(a, "msg-3").await,
        world.publish_included(a, "msg-4").await,
    ];
    world.finalize_all(&hashes).await;

    world.restart(a, Some(stale)).await;
    world.steps(2).await;
    let hash = world.publish_included(a, "msg-5").await;
    world.finalize_tx(hash).await;
    world.settle_chain(40).await;

    let expected = ["msg-1", "msg-2", "msg-3", "msg-4", "msg-5"];
    let payloads = world.indexer_payloads().await;
    assert_eq!(filtered(&payloads, &expected), strs(&expected));
    world.finish();
}
scenario!(
    s03_resume_from_stale_checkpoint,
    resume_from_stale_checkpoint
);

/// S4: publishes fail fast while the node is down and work after reconnect.
async fn publish_while_node_down(network: Network) {
    let mut world = World::new(network);
    let a = world.start("SEQ_A", passive()).await;

    world.node().node_down();
    world.harness.settle().await;
    assert!(
        world.publish(a, "while down").await.is_err(),
        "publish must fail while the node is down"
    );
    world.node().node_up();

    let mut hashes = Vec::new();
    for payload in ["After down (1)", "After down (2)", "After down (3)"] {
        let mut result = None;
        for _ in 0..20 {
            match world.publish(a, payload).await {
                Ok(r) => {
                    result = Some(r);
                    break;
                }
                Err(_) => world.harness.advance(RECONNECT_DELAY).await,
            }
        }
        let hash = result
            .expect("publish accepted after reconnect")
            .inscription_id();
        world.include(hash).await;
        hashes.push(hash);
    }
    world.finalize_all(&hashes).await;

    let expected = ["After down (1)", "After down (2)", "After down (3)"];
    let payloads = world.indexer_payloads().await;
    assert_eq!(
        filtered_first_occurrence(&payloads, &expected),
        strs(&expected)
    );
    world.finish();
}
scenario!(s04_publish_while_node_down, publish_while_node_down);

/// S5: two instances of the same key publish one after the other.
async fn sequential_same_key(network: Network) {
    let mut world = World::new(network);
    let a = world.start("SEQ_A", passive()).await;
    let mut hashes = Vec::new();
    for payload in ["a1", "a2", "a3"] {
        hashes.push(world.publish_included(a, payload).await);
    }
    world.finalize_all(&hashes).await;
    world.stop(a);

    let b = world.start_as("SEQ_B", seq_key("SEQ_A"), passive()).await;
    let mut hashes = Vec::new();
    for payload in ["b1", "b2", "b3"] {
        hashes.push(world.publish_included(b, payload).await);
    }
    world.finalize_all(&hashes).await;
    world.stop(b);

    world.restart(a, None).await;
    let mut hashes = Vec::new();
    for payload in ["a4", "a5", "a6"] {
        hashes.push(world.publish_included(a, payload).await);
    }
    world.finalize_all(&hashes).await;

    let expected = ["a1", "a2", "a3", "b1", "b2", "b3", "a4", "a5", "a6"];
    let payloads = world.indexer_payloads().await;
    assert_eq!(filtered(&payloads, &expected), strs(&expected));
    world.finish();
}
scenario!(s05_sequential_same_key, sequential_same_key);

/// S6: three same-key writers race 20 messages each and converge.
async fn concurrent_same_key(network: Network) {
    let mut world = World::with_funding(network, 200, 3000);
    let a = world.start("SEQ_A", passive()).await;
    world.stop(a);

    let mut expected = Vec::new();
    for (name, prefix) in [("SEQ_A", "a"), ("SEQ_B", "b"), ("SEQ_C", "c")] {
        let planned: Vec<String> = (1..=20).map(|i| format!("{prefix}{i}")).collect();
        expected.extend(planned.clone());
        let config = world.config_with_depth(usize::MAX);
        let planned = planned.iter().map(|p| inscription(p)).collect();
        let policy = ReferenceConsumer::boxed(&seq_key("SEQ_A"), planned);
        world
            .start_as_with(name, seq_key("SEQ_A"), config, policy)
            .await;
    }
    world.settle_chain(300).await;

    let expected: Vec<&str> = expected.iter().map(String::as_str).collect();
    let payloads = world.indexer_payloads().await;
    assert_exactly_once_any_order(&payloads, &expected);
    world.finish();
}
scenario!(s06_concurrent_same_key, concurrent_same_key);

/// S7: the sorted-conflict policy keeps per-sequencer order and converges.
async fn sorted_conflict(network: Network) {
    let mut world = World::new(network);
    let a = world.start("SEQ_A", passive()).await;
    world.config_finalized(a, &["SEQ_B"], 10, 0).await;
    world.stop(a);

    let discarded = Arc::new(Mutex::new(HashSet::new()));
    world.restart(a, None).await;
    world
        .harness
        .set_policy(a, Box::new(SortedConflictPolicy::new(&discarded)));
    let b = world
        .start("SEQ_B", Box::new(SortedConflictPolicy::new(&discarded)))
        .await;

    let a_payloads = ["aa", "cc", "ee", "gg", "ii"];
    let b_payloads = ["bb", "dd", "ff", "hh", "jj"];
    for i in 0..5 {
        world.publish(a, a_payloads[i]).await.expect("publish");
        world.publish(b, b_payloads[i]).await.expect("publish");
    }
    world.settle_chain(300).await;

    let expected: Vec<&str> = a_payloads
        .iter()
        .chain(b_payloads.iter())
        .copied()
        .collect();
    let payloads = world.indexer_payloads().await;
    let on_chain = filtered_first_occurrence(&payloads, &expected);
    let discarded: HashSet<String> = discarded
        .lock()
        .unwrap()
        .iter()
        .map(|p| String::from_utf8_lossy(p.as_slice()).into_owned())
        .collect();
    assert!(!on_chain.is_empty(), "nothing landed");
    assert!(
        on_chain.iter().all(|p| !discarded.contains(p)),
        "a discarded payload landed: {on_chain:?} vs {discarded:?}"
    );
    assert_eq!(
        on_chain.len() + discarded.len(),
        10,
        "on chain {on_chain:?}, discarded {discarded:?}"
    );
    for list in [a_payloads, b_payloads] {
        let survivors: Vec<usize> = on_chain
            .iter()
            .filter_map(|p| list.iter().position(|x| x == p))
            .collect();
        assert!(
            survivors.windows(2).all(|w| w[0] < w[1]),
            "per-sequencer order broken: {on_chain:?}"
        );
    }
    world.finish();
}
scenario!(s07_sorted_conflict, sorted_conflict);

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

fn count_payload(payloads: &[String], payload: &str) -> usize {
    payloads.iter().filter(|p| p.as_str() == payload).count()
}

/// S8: three independent sequencers run the 2-of-3 deposit lifecycle.
async fn multisig_deposit_lifecycle(network: Network) {
    let mut world = World::new(network);
    world.coin_split(&[5, 5, 5]).await;
    let bus: MultiSigBus = Arc::new(Mutex::new(std::collections::HashMap::new()));
    let (announce, _keep): (BundleAnnounce, _) = tokio::sync::broadcast::channel(256);
    let recipient = world.funding_pk();
    let mut indices = Vec::new();
    for name in ["SEQ_A", "SEQ_B", "SEQ_C"] {
        let policy = MultiSigLifecyclePolicy::new(
            vec![3],
            recipient,
            Arc::clone(&bus),
            announce.clone(),
            seq_key(name),
        );
        indices.push(world.start(name, Box::new(policy)).await);
    }
    let a = indices[0];
    let b = indices[1];

    escalate_config(&mut world, a, a, &["SEQ_A"], 1, 0, &[]).await;
    escalate_config(&mut world, a, a, &["SEQ_A", "SEQ_B"], 2, 0, &["SEQ_A"]).await;
    escalate_config(
        &mut world,
        a,
        b,
        &["SEQ_A", "SEQ_B", "SEQ_C"],
        2,
        15,
        &["SEQ_B", "SEQ_A"],
    )
    .await;

    let notes = world.take_user_notes(&[5]);
    let (_, op, _) = world.deposit(&notes, "Mint 5 to channel").await;
    let pin =
        String::from_utf8_lossy(pin_payload(&lb_core::mantle::ops::OpId::op_id(&op)).as_slice())
            .into_owned();
    let withdraw = String::from_utf8_lossy(
        withdraw_payload(&lb_core::mantle::ops::OpId::op_id(&op)).as_slice(),
    )
    .into_owned();

    world.settle_chain(400).await;
    let payloads = world.indexer_payloads().await;
    assert_eq!(count_payload(&payloads, &pin), 1, "pins");
    assert_eq!(count_payload(&payloads, &withdraw), 1, "withdraws");
    let wallet = world.wallet(a);
    assert_eq!(wallet.finalized.len(), 1, "finalized notes {wallet:?}");
    assert_eq!(wallet.unfinalized.len(), 0, "unfinalized notes {wallet:?}");
    assert!(wallet.finalized.iter().any(|n| n.value == 2), "{wallet:?}");
    world.finish();
}
scenario!(s08_multisig_deposit_lifecycle, multisig_deposit_lifecycle);

fn not_our_turn_with_pending(world: &mut World, index: usize, pending: usize) -> bool {
    let view = world.view(index);
    view.own_key_index == Some(1)
        && view.authorized_key_index.is_some()
        && !view.our_turn_to_write
        && view.authorized_key_index != view.own_key_index
        && view.pending_publish_txs == pending
}

async fn round_robin_queue(network: Network, depth: usize, payloads: [&str; 3], unlimited: bool) {
    let mut world = World::new(network);
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
    let included = if unlimited { 3 } else { 2 };
    for hash in &hashes[..included] {
        world.include(*hash).await;
    }
    if unlimited {
        assert!(
            world.step_until(60, |w| w.pending_count(b) == 0).await,
            "pending never drained"
        );
    } else {
        assert!(
            world.step_until(60, |w| w.pending_count(b) == 1).await,
            "pending never dropped to one"
        );
    }
    let payloads_on_chain = world.indexer_payloads().await;
    assert_exactly_once_any_order(&payloads_on_chain, &payloads[..included]);
    world.finish();
}

/// S9: a depth-limited sequencer waits for its turn and submits pending.
async fn round_robin_depth_two(network: Network) {
    round_robin_queue(
        network,
        2,
        ["rr-queued-b1", "rr-queued-b2", "rr-queued-b3"],
        false,
    )
    .await;
}
scenario!(s09_round_robin_depth_two, round_robin_depth_two);

/// S10: with no depth limit every pending message is submitted on turn.
async fn round_robin_unlimited(network: Network) {
    round_robin_queue(
        network,
        usize::MAX,
        ["rr-unbounded-1", "rr-unbounded-2", "rr-unbounded-3"],
        true,
    )
    .await;
}
scenario!(s10_round_robin_unlimited, round_robin_unlimited);

/// S11: a config landing orphans pending inscriptions which re-adopt in order.
async fn config_orphans_pending(network: Network) {
    let mut world = World::new(network);
    let config = world.config_with_depth(usize::MAX);
    let a = world.start_with("SEQ_A", config.clone(), passive()).await;
    world.config_finalized(a, &["SEQ_B"], 2, 0).await;
    let b = world.start_with("SEQ_B", config, reference("SEQ_B")).await;
    assert!(
        world
            .step_until(40, |w| not_our_turn_with_pending(w, b, 0))
            .await
    );
    for payload in ["cfg-orphan-b1", "cfg-orphan-b2", "cfg-orphan-b3"] {
        world.publish(b, payload).await.expect("queued");
    }
    assert!(
        world
            .step_until(40, |w| not_our_turn_with_pending(w, b, 3))
            .await
    );
    world.config_finalized(a, &["SEQ_B"], 2, 0).await;
    world.settle_chain(200).await;

    let expected = ["cfg-orphan-b1", "cfg-orphan-b2", "cfg-orphan-b3"];
    let payloads = world.indexer_payloads().await;
    assert_eq!(
        filtered_first_occurrence(&payloads, &expected),
        strs(&expected)
    );
    world.finish();
}
scenario!(s11_config_orphans_pending, config_orphans_pending);

/// S12: a sequencer publishes immediately when it is its turn.
async fn publish_in_turn(network: Network) {
    let mut world = World::new(network);
    let a = world.start("SEQ_A", passive()).await;
    world.config_finalized(a, &["SEQ_B"], 2, 0).await;
    let b = world.start("SEQ_B", passive()).await;

    world.wait_turn(a).await;
    world.wait_turn(b).await;
    world.wait_turn(a).await;
    let hash = world
        .publish(a, "decentralized-immediate-publish")
        .await
        .expect("publish")
        .inscription_id();
    world.include(hash).await;
    world.finalize_tx(hash).await;
    let payloads = world.indexer_payloads().await;
    assert_exactly_once_any_order(&payloads, &["decentralized-immediate-publish"]);
    world.finish();
}
scenario!(s12_publish_in_turn, publish_in_turn);

/// S13: sequencers are added to the rotation one config at a time.
async fn dynamic_sequencers(network: Network) {
    let mut world = World::new(network);
    let a = world.start("SEQ_A", reference("SEQ_A")).await;
    world.config_finalized(a, &[], 2, 0).await;
    world.wait_turn(a).await;
    world.publish(a, "seq_a-msg1").await.expect("publish");

    world.config_finalized(a, &["SEQ_B"], 2, 0).await;
    let b = world.start("SEQ_B", reference("SEQ_B")).await;
    world.wait_turn(b).await;
    world.publish(b, "seq_b-msg1").await.expect("publish");

    world.config_finalized(a, &["SEQ_B", "SEQ_C"], 2, 0).await;
    let c = world.start("SEQ_C", reference("SEQ_C")).await;
    world.wait_turn(c).await;
    world.publish(c, "seq_c-msg1").await.expect("publish");

    world.publish(a, "seq_a-msg2").await.expect("publish");
    world.publish(b, "seq_b-msg2").await.expect("publish");
    world.publish(c, "seq_c-msg2").await.expect("publish");
    world.settle_chain(200).await;

    let expected = [
        "seq_a-msg1",
        "seq_b-msg1",
        "seq_c-msg1",
        "seq_a-msg2",
        "seq_b-msg2",
        "seq_c-msg2",
    ];
    let payloads = world.indexer_payloads().await;
    assert_exactly_once_any_order(&payloads, &expected);
    world.finish();
}
scenario!(s13_dynamic_sequencers, dynamic_sequencers);

/// A planned balance update: message alias, account and delta.
type BalanceUpdate = (&'static str, &'static str, i64);

/// S14: balance-aware republishing never drives an account negative.
async fn balance_aware(network: Network) {
    let mut world = World::new(network);
    let a = world.start("SEQ_A", passive()).await;
    world.config_finalized(a, &["SEQ_B", "SEQ_C"], 60, 0).await;
    world.stop(a);

    let balances: std::collections::HashMap<String, i64> =
        [("alice", 10), ("bob", 10), ("charlie", 10)]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect();
    let plans: [(&str, [BalanceUpdate; 3]); 3] = [
        (
            "SEQ_A",
            [
                ("a-alice", "alice", -6),
                ("a-bob", "bob", -3),
                ("a-charlie", "charlie", -2),
            ],
        ),
        (
            "SEQ_B",
            [
                ("b-alice", "alice", -5),
                ("b-bob", "bob", -4),
                ("b-charlie", "charlie", -8),
            ],
        ),
        (
            "SEQ_C",
            [
                ("c-alice", "alice", -4),
                ("c-bob", "bob", -7),
                ("c-charlie", "charlie", -1),
            ],
        ),
    ];
    let mut expected = Vec::new();
    for (name, plan) in plans {
        let planned: Vec<_> = plan
            .iter()
            .map(|(uuid, account, delta)| balance_update_payload(uuid, account, *delta))
            .collect();
        expected.extend(planned.iter().map(payload_str));
        let config = world.default_config();
        let balances = balances.clone();
        world
            .start_then(name, seq_key(name), config, move |seq, _| {
                Box::new(BalanceAwarePolicy::new(seq, balances, planned))
            })
            .await;
    }
    // Every sequencer needs a turn: three rotations of 60 slots.
    world.steps(190).await;
    world.settle_chain(200).await;

    let expected: Vec<&str> = expected.iter().map(String::as_str).collect();
    let payloads = world.indexer_payloads().await;
    let mut balances = balances;
    for payload in filtered(&payloads, &expected) {
        let (_, account, delta) =
            parse_balance_payload(&inscription(&payload)).expect("balance payload");
        *balances.get_mut(&account).expect("known account") += delta;
    }
    assert!(
        balances.values().all(|v| *v >= 0),
        "an account went negative: {balances:?}"
    );
    world.finish();
}
scenario!(s14_balance_aware, balance_aware);

/// S15: identical payloads published concurrently each land exactly once.
async fn identical_payloads(network: Network) {
    let mut world = World::with_funding(network, 200, 3000);
    let a = world.start("SEQ_A", passive()).await;
    world.config_finalized(a, &["SEQ_B", "SEQ_C"], 60, 0).await;
    world.stop(a);
    for name in ["SEQ_A", "SEQ_B", "SEQ_C"] {
        let config = world.config_with_depth(usize::MAX);
        let planned = vec![inscription("shared-message"); 10];
        world
            .start_as_with(
                name,
                seq_key(name),
                config,
                ReferenceConsumer::boxed(&seq_key(name), planned),
            )
            .await;
    }
    world.steps(190).await;
    world.settle_chain(200).await;

    let payloads = world.indexer_payloads().await;
    let copies = payloads
        .iter()
        .filter(|p| p.as_str() == "shared-message")
        .count();
    assert_eq!(copies, 30, "copies of the shared message");
    world.finish();
}
scenario!(s15_identical_payloads, identical_payloads);

fn wallet_values(view: &lb_zone_sdk::sequencer::ChannelWalletView) -> (Vec<u64>, Vec<u64>) {
    (
        view.finalized.iter().map(|n| n.value).collect(),
        view.unfinalized.iter().map(|n| n.value).collect(),
    )
}

/// S16: the channel wallet tracks deposited notes with exact values.
async fn channel_wallet_tracks_deposits(network: Network) {
    let mut world = World::new(network);
    world.coin_split(&[1, 2]).await;
    let a = world.start("SEQ_A", passive()).await;
    world.publish_included(a, "initial inscription").await;

    let notes = world.take_user_notes(&[1, 2]);
    let (hash, _, _) = world.deposit(&notes, "Mint 1 and 2 into the channel").await;
    world.include(hash).await;
    let (finalized, unfinalized) = wallet_values(&world.wallet(a));
    let all: Vec<u64> = finalized
        .iter()
        .chain(unfinalized.iter())
        .copied()
        .collect();
    assert!(all.contains(&1) && all.contains(&2), "wallet {all:?}");

    world.finalize_tx(hash).await;
    let (finalized, unfinalized) = wallet_values(&world.wallet(a));
    assert!(
        finalized.contains(&1) && finalized.contains(&2),
        "{finalized:?}"
    );
    assert_eq!(finalized.len(), 2);
    assert!(unfinalized.is_empty(), "{unfinalized:?}");

    let checkpoint = world.checkpoint(a);
    world.restart(a, Some(checkpoint)).await;
    let (finalized, unfinalized) = wallet_values(&world.wallet(a));
    assert_eq!(
        (finalized.len(), unfinalized.len()),
        (2, 0),
        "after checkpoint restart"
    );

    world.restart(a, None).await;
    let (finalized, unfinalized) = wallet_values(&world.wallet(a));
    assert_eq!(
        (finalized.len(), unfinalized.len()),
        (2, 0),
        "after fresh restart"
    );
    world.finish();
}
scenario!(
    s16_channel_wallet_tracks_deposits,
    channel_wallet_tracks_deposits
);

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

/// S17: finalized deposits are returned by the indexer.
async fn finalized_deposit_indexed(network: Network) {
    let mut world = World::new(network);
    world.coin_split(&[1, 1, 1]).await;
    let a = world.start("SEQ_A", passive()).await;
    world.publish_included(a, "initial inscription").await;
    let notes = world.take_user_notes(&[1]);
    let (hash, op, _) = world.deposit(&notes, "Mint 1 to Alice in Zone").await;
    world.include(hash).await;
    world.finalize_tx(hash).await;
    let finalized = world.indexer_finalized().await;
    assert!(
        finalized_deposit(&finalized, &op, 1).is_some(),
        "deposit not indexed"
    );
    world.finish();
}
scenario!(s17_finalized_deposit_indexed, finalized_deposit_indexed);

/// Fee margin of the cucumber atomic deposit step; the mandatory fee varies
/// with input count and change.
const ATOMIC_DEPOSIT_FEE_MARGIN: u64 = 10_000;
const ATOMIC_DEPOSIT_FUNDING: u64 = 20_000;

/// S18: a deposit and its inscription are finalized together in one tx.
async fn atomic_deposit_and_inscription(network: Network) {
    let mut world = World::new(network);
    world.coin_split(&[ATOMIC_DEPOSIT_FUNDING]).await;
    let a = world.start("SEQ_A", passive()).await;
    let first = world.publish_included(a, "initial inscription").await;
    world.finalize_tx(first).await;

    // Like the cucumber step: the user's transfer creates the deposit note
    // and leaves a generous fee margin unspent.
    let utxo = world.take_user_notes(&[ATOMIC_DEPOSIT_FUNDING])[0];
    let user_pk = world.user_pk();
    let change = ATOMIC_DEPOSIT_FUNDING - 1 - ATOMIC_DEPOSIT_FEE_MARGIN;
    let transfer = TransferOp::new(
        Inputs::new(utxo.id()),
        Outputs::new([Note::new(1, user_pk), Note::new(change, user_pk)]),
    );
    let deposited = Utxo::new(transfer.op_id(), 0, Note::new(1, user_pk));
    let deposit = DepositOp {
        channel_id: world.channel_id(),
        inputs: Inputs::new(deposited.id()),
        metadata: b"Mint 1 to Alice in Zone"
            .to_vec()
            .try_into()
            .expect("metadata"),
    };
    let ops = Ops::try_from(vec![
        Op::Transfer(transfer),
        Op::ChannelDeposit(deposit.clone()),
    ])
    .expect("ops");
    let (ops, msg_id, signature) = world
        .harness
        .seq(a)
        .handle()
        .prepare_tx(ops, inscription("Mint 1 to Alice"))
        .expect("prepared");
    let proofs = OpProofs::try_from(vec![
        OpProof::ZkSig(ZkSignature::sample()),
        OpProof::ZkSig(ZkSignature::sample()),
        OpProof::Ed25519Sig(signature),
    ])
    .expect("proofs");
    let signed = SignedOps::from_parts(ops, proofs).expect("signed");
    let (result, _) = world
        .harness
        .seq(a)
        .handle()
        .submit_signed_tx(signed, msg_id)
        .expect("submitted");
    world.harness.settle().await;
    let hash = result.inscription_id();
    world.include(hash).await;
    world.finalize_tx(hash).await;

    let finalized = world.indexer_finalized().await;
    assert!(
        finalized_deposit(&finalized, &deposit, 1).is_some(),
        "deposit not indexed"
    );
    let payloads = common::inscription_payloads(&finalized);
    let expected = ["initial inscription", "Mint 1 to Alice"];
    assert_eq!(
        filtered_first_occurrence(&payloads, &expected),
        strs(&expected)
    );
    world.finish();
}
scenario!(
    s18_atomic_deposit_and_inscription,
    atomic_deposit_and_inscription
);

/// S19: finalized withdraws reach both the indexer and the sequencer.
async fn finalized_withdraw(network: Network) {
    let mut world = World::new(network);
    world.coin_split(&[3, 3, 3]).await;
    let a = world.start("SEQ_A", passive()).await;
    let first = world.publish_included(a, "initial inscription").await;
    world.finalize_tx(first).await;
    let notes = world.take_user_notes(&[3]);
    let (hash, op, _) = world.deposit(&notes, "Mint 3 to Alice in Zone").await;
    world.include(hash).await;
    world.finalize_tx(hash).await;

    let user_pk = world.user_pk();
    let (result, _) = world
        .harness
        .seq(a)
        .handle()
        .publish_atomic_withdraw(
            inscription("Burn 2"),
            vec![WithdrawArg {
                outputs: Outputs::new([Note::new(2, user_pk)]),
            }],
            WithdrawInputs::Auto,
        )
        .await
        .expect("withdraw accepted");
    world.harness.settle().await;
    let PendingTx::AtomicWithdraw(info) = &result.tx else {
        panic!("expected an atomic withdraw, got {:?}", result.tx);
    };
    let withdraw_op = info.withdraws[0].op.clone();
    let withdraw_hash = result.inscription_id();
    world.include(withdraw_hash).await;
    world.finalize_tx(withdraw_hash).await;

    let finalized = world.indexer_finalized().await;
    assert!(
        finalized_deposit(&finalized, &op, 3).is_some(),
        "deposit not indexed"
    );
    let via_events = world.finalized_via_events(a);
    assert!(
        finalized_deposit(&via_events, &op, 3).is_some(),
        "deposit not seen by SEQ_A"
    );
    let has_withdraw = |txs: &[lb_zone_sdk::sequencer::FinalizedTx]| {
        txs.iter()
            .flat_map(|tx| tx.ops.iter())
            .any(|f| matches!(f, FinalizedOp::Withdraw(w) if w.op.inputs == withdraw_op.inputs))
    };
    assert!(has_withdraw(&finalized), "withdraw not indexed");
    assert!(has_withdraw(&via_events), "withdraw not seen by SEQ_A");
    let payloads = common::inscription_payloads(&finalized);
    let expected = ["initial inscription", "Burn 2"];
    assert_eq!(
        filtered_first_occurrence(&payloads, &expected),
        strs(&expected)
    );
    world.finish();
}
scenario!(s19_finalized_withdraw, finalized_withdraw);

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

/// S20: an observed deposit is pinned then withdrawn before finalization.
async fn observed_deposit_pinned_and_withdrawn(network: Network) {
    let mut world = World::new(network);
    world.coin_split(&[5, 5, 5]).await;
    let a = world.start("SEQ_A", passive()).await;
    let first = world.publish_included(a, "initial inscription").await;
    world.finalize_tx(first).await;
    let recipient = world.funding_pk();
    let b = world
        .start_as(
            "SEQ_B",
            seq_key("SEQ_A"),
            Box::new(DepositLifecyclePolicy::new(vec![1, 1, 2], recipient)),
        )
        .await;
    world.publish_included(a, "a1").await;
    world.publish_included(a, "a2").await;

    let notes = world.take_user_notes(&[5]);
    let (hash, _, _) = world.deposit(&notes, "Mint 5 for atomic integration").await;
    world.include(hash).await;
    world.settle_chain(200).await;

    let (finalized, _) = wallet_values(&world.wallet(b));
    assert!(finalized.contains(&1), "SEQ_B wallet {finalized:?}");
    let indexed = world.indexer_finalized().await;
    assert!(
        finalized_transfer_input_counts(&indexed).contains(&1),
        "no finalized channel transfer consuming 1 input"
    );
    let payloads = common::inscription_payloads(&indexed);
    let expected = ["initial inscription", "a1", "a2"];
    assert_eq!(
        filtered_first_occurrence(&payloads, &expected),
        strs(&expected)
    );
    world.finish();
}
scenario!(
    s20_observed_deposit_pinned_and_withdrawn,
    observed_deposit_pinned_and_withdrawn
);

/// S21: a reactive withdrawal sweeps dust and stays within 255 inputs.
async fn withdraw_under_dust_flood(network: Network) {
    let mut world = World::with_funding(network, 60, 2000);
    world.coin_split(&[255, 10000]).await;
    let recipient = world.funding_pk();
    let a = world
        .start(
            "SEQ_A",
            Box::new(DepositWithdrawPolicy::new(10000, vec![10000], recipient)),
        )
        .await;
    let first = world.publish_included(a, "initial inscription").await;
    world.finalize_tx(first).await;

    let seed = world.take_user_notes(&[255]);
    let (hash, _, channel_notes) = world.deposit(&seed, "Dust seed").await;
    world.include(hash).await;
    assert!(
        world
            .step_until(40, |w| {
                let (f, u) = wallet_values(&w.wallet(a));
                f.contains(&255) || u.contains(&255)
            })
            .await,
        "seed note not in the wallet"
    );
    let user_pk = world.user_pk();
    let split = txs::build_channel_split_tx(
        world.node(),
        world.channel_id(),
        &seq_key("SEQ_A"),
        recipient,
        channel_notes[0],
        user_pk,
        255,
    )
    .await
    .expect("split built");
    let split_hash = lb_core::mantle::traits::Hashable::hash(&split);
    world.node().submit(split);
    world.include(split_hash).await;
    world.finalize_tx(split_hash).await;

    let big = world.take_user_notes(&[10000]);
    let (hash, _, _) = world.deposit(&big, "Big note").await;
    world.include(hash).await;
    world.settle_chain(200).await;

    let indexed = world.indexer_finalized().await;
    assert!(
        finalized_transfer_input_counts(&indexed).contains(&255),
        "no finalized channel transfer consuming 255 inputs: {:?}",
        finalized_transfer_input_counts(&indexed)
    );
    world.finish();
}
scenario!(s21_withdraw_under_dust_flood, withdraw_under_dust_flood);

/// S22: concurrent custom multi-inscription txs recover from conflicts.
async fn custom_multi_inscription(network: Network) {
    let mut world = World::with_funding(network, 200, 3000);
    let a = world.start("SEQ_A", passive()).await;
    world.config_finalized(a, &["SEQ_B"], 10, 0).await;
    world.stop(a);

    let funding_pk = world.funding_pk();
    let channel_id = world.channel_id();
    let mut expected = Vec::new();
    for (name, tag) in [("SEQ_A", "a"), ("SEQ_B", "b")] {
        let batches: Vec<Vec<_>> = (0..5)
            .map(|tx| {
                (0..5)
                    .map(|entry| inscription(&format!("custom-{tag}-{tx}-{entry}")))
                    .collect()
            })
            .collect();
        expected.extend(batches.iter().flatten().map(payload_str));
        let config = world.default_config();
        let key = seq_key(name);
        world
            .start_then(name, key.clone(), config, move |seq, node| {
                Box::new(CustomRepublishPolicy::new(
                    seq,
                    node.clone(),
                    channel_id,
                    key,
                    funding_pk,
                    batches,
                ))
            })
            .await;
    }
    world.settle_chain(400).await;

    let payloads = world.indexer_payloads().await;
    let found: HashSet<&str> = payloads.iter().map(String::as_str).collect();
    let missing: Vec<&String> = expected
        .iter()
        .filter(|p| !found.contains(p.as_str()))
        .collect();
    assert!(missing.is_empty(), "missing custom payloads: {missing:?}");
    world.finish();
}
scenario!(s22_custom_multi_inscription, custom_multi_inscription);

/// S23: a non-leading sequencer re-keys the channel; in-flight inscriptions
/// recover.
async fn rekey_by_non_leader(network: Network) {
    let mut world = World::new(network);
    let a = world.start("SEQ_A", reference("SEQ_A")).await;
    world.publish_included(a, "inscription before re-key").await;
    world.config_finalized(a, &["SEQ_B"], 2, 0).await;
    let b = world.start("SEQ_B", reference("SEQ_B")).await;
    world
        .publish(a, "in flight during re-key")
        .await
        .expect("publish");
    let rekey = world.submit_config(b, &["SEQ_A"], 2, 0).await;
    world
        .publish(b, "post re-key from B")
        .await
        .expect("publish");
    world
        .publish(a, "post re-key from A")
        .await
        .expect("publish");
    world.include(rekey).await;
    world.settle_chain(200).await;

    let expected = [
        "inscription before re-key",
        "in flight during re-key",
        "post re-key from B",
        "post re-key from A",
    ];
    let payloads = world.indexer_payloads().await;
    assert_exactly_once_any_order(&payloads, &expected);
    world.finish();
}
scenario!(s23_rekey_by_non_leader, rekey_by_non_leader);

fn fee_notes(
    checkpoint: &lb_zone_sdk::sequencer::SequencerCheckpoint,
    hash: &lb_zone_sdk::node_types::TxHash,
) -> Vec<lb_core::mantle::NoteId> {
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

/// S24: a reorg after a node restart double-hands a fee note; the loser is
/// re-funded and both inscriptions finalize once.
async fn refund_after_node_restart(network: Network) {
    let mut world = World::with_funding_values(network, &[9000, 8000]);
    let config = SequencerConfig {
        stale_refund_slots: 4,
        ..world.default_config()
    };
    let a = world.start_with("SEQ_A", config, passive()).await;

    let first = world
        .publish(a, "message 1")
        .await
        .expect("publish")
        .inscription_id();
    world.include(first).await;
    let mined_in = world.node().block_of_tx(&first).expect("included");
    let fork_parent = world.node().block(mined_in).expect("block").parent;

    world.node().node_down();
    world.harness.settle().await;
    let tip_slot = world.node().tip_slot().into_inner();
    let mut parent = fork_parent;
    for i in 1..=2 {
        parent = world.node().mine_block(BlockSpec {
            parent: Some(parent),
            slot: Some((tip_slot + i).into()),
            txs: TxSelection::None,
            ..Default::default()
        });
    }
    assert!(
        !world.node().is_included(&first),
        "fork must drop message 1"
    );
    world.node().node_up();
    for _ in 0..10 {
        world.harness.advance(RECONNECT_DELAY).await;
        if world.node().open_streams() > 0 {
            break;
        }
    }
    let second = world.publish_retry(a, "message 2").await;
    let checkpoint = world.checkpoint(a);
    let first_notes = fee_notes(&checkpoint, &first);
    let second_notes = fee_notes(&checkpoint, &second);
    let overlap = first_notes.iter().any(|n| second_notes.contains(n));
    world.restart(a, Some(checkpoint)).await;
    world.settle_chain(200).await;

    let payloads = world.indexer_payloads().await;
    assert_exactly_once_any_order(&payloads, &["message 1", "message 2"]);
    assert!(
        overlap,
        "the two publishes did not share a fee note: {first_notes:?} vs {second_notes:?}"
    );
    let winners = [first, second]
        .iter()
        .filter(|h| world.node().is_tx_finalized(h))
        .count();
    assert_eq!(
        winners, 1,
        "exactly one double-handed tx finalizes under its submitted hash"
    );
    world.finish();
}
scenario!(s24_refund_after_node_restart, refund_after_node_restart);

/// Slot of the block that included `hash`.
fn included_slot(world: &World, hash: &lb_zone_sdk::node_types::TxHash) -> u64 {
    let node = world.node();
    node.block(node.block_of_tx(hash).expect("included"))
        .expect("block")
        .slot
        .into_inner()
}

/// S25: with no timeframe, a silent tip sequencer loses the turn after the
/// posting timeout and the waiting sequencer's queued message lands.
async fn timeout_rotates_silent_turn(network: Network) {
    let mut world = World::new(network);
    let a = world.start("SEQ_A", passive()).await;
    world.config_finalized(a, &["SEQ_B"], 0, 3).await;
    let b = world.start("SEQ_B", passive()).await;

    let a1 = world.publish_included(a, "a1").await;
    let b1 = world
        .publish(b, "b1")
        .await
        .expect("queued")
        .inscription_id();
    world.include(b1).await;
    assert!(
        included_slot(&world, &b1) >= included_slot(&world, &a1) + 3,
        "b1 landed before SEQ_A's turn timed out"
    );

    let a2 = world
        .publish(a, "a2")
        .await
        .expect("queued")
        .inscription_id();
    world.include(a2).await;
    assert!(
        included_slot(&world, &a2) >= included_slot(&world, &b1) + 3,
        "a2 landed before SEQ_B's turn timed out"
    );
    world.settle_chain(200).await;
    let payloads = world.indexer_payloads().await;
    assert_eq!(
        filtered_first_occurrence(&payloads, &["a1", "b1", "a2"]),
        strs(&["a1", "b1", "a2"])
    );
    world.finish();
}
scenario!(s25_timeout_rotates_silent_turn, timeout_rotates_silent_turn);

/// S26: once the tip is silent past the timeout, the timeout grid overrides
/// the timeframe rotation; a sequencer two places down waits for its timeout
/// window and still publishes.
async fn timeout_overrides_timeframe(network: Network) {
    let mut world = World::new(network);
    let a = world.start("SEQ_A", passive()).await;
    world.config_finalized(a, &["SEQ_B", "SEQ_C"], 4, 5).await;
    let c = world.start("SEQ_C", passive()).await;

    let a1 = world.publish_included(a, "a1").await;
    let c1 = world
        .publish(c, "c1")
        .await
        .expect("queued")
        .inscription_id();
    world.include_within(c1, 60).await;
    // SEQ_B never posts, so after the timeout the turn goes B then C on the
    // timeout grid from a1's slot.
    let (a1_slot, c1_slot) = (included_slot(&world, &a1), included_slot(&world, &c1));
    assert!(
        c1_slot >= a1_slot + 2 * 5,
        "c1 landed at slot {c1_slot} before SEQ_C's timeout window (a1 at {a1_slot})"
    );
    world.settle_chain(200).await;
    let payloads = world.indexer_payloads().await;
    assert_eq!(
        filtered_first_occurrence(&payloads, &["a1", "c1"]),
        strs(&["a1", "c1"])
    );
    world.finish();
}
scenario!(s26_timeout_overrides_timeframe, timeout_overrides_timeframe);

/// S27: an accredited key driven outside the SDK posts in its own turns; the
/// SDK sequencer adopts those entries and keeps chaining after them.
async fn foreign_sequencer_writes(network: Network) {
    let mut world = World::new(network);
    let a = world.start("SEQ_A", reference("SEQ_A")).await;
    world.config_finalized(a, &["SEQ_C"], 3, 0).await;
    let foreign_key = seq_key("SEQ_C");
    let foreign_index = 1u16;
    let funding_pk = world.funding_pk();
    let channel_id = world.channel_id();

    let ours = ["a0", "a1", "a2"];
    let theirs = ["x0", "x1", "x2"];
    for payload in ours {
        world.publish(a, payload).await.expect("queued");
    }
    let mut queue: Vec<&str> = theirs.to_vec();
    let mut in_flight: Option<(lb_zone_sdk::node_types::TxHash, &str)> = None;
    for _ in 0..120 {
        if let Some((hash, payload)) = in_flight {
            let node = world.node();
            let in_mempool = |n: &logos_blockchain_zone_sdk_testing::SimNode| {
                n.mempool().iter().any(|tx| tx.hash() == hash)
            };
            let known_to_peer = world
                .peer
                .as_ref()
                .is_some_and(|peer| peer.is_included(&hash) || in_mempool(peer));
            if node.is_included(&hash) {
                in_flight = None;
            } else if !in_mempool(node) && !known_to_peer {
                // Dropped everywhere (a fork or a lost turn): post it again.
                queue.insert(0, payload);
                in_flight = None;
            }
        }
        if in_flight.is_none() && !queue.is_empty() {
            let node = world.node();
            let state = node.channel_state_at_tip(channel_id).expect("channel");
            let next_slot = node.tip_slot().into_inner() + 1;
            if state.round_robin(next_slot.into()).0 == foreign_index {
                let payload = queue.remove(0);
                let (tx, _) = txs::build_funded_custom_tx(
                    node,
                    channel_id,
                    &foreign_key,
                    funding_pk,
                    &[inscription(payload)],
                    state.tip_message,
                )
                .await
                .expect("foreign tx built");
                let hash = tx.hash();
                node.submit(tx);
                in_flight = Some((hash, payload));
            }
        }
        if queue.is_empty() && in_flight.is_none() && world.pending_count(a) == 0 {
            break;
        }
        world.step().await;
    }
    world.settle_chain(200).await;

    let payloads = world.indexer_payloads().await;
    let mut expected: Vec<&str> = ours.to_vec();
    expected.extend(theirs);
    assert_exactly_once_any_order(&payloads, &expected);
    let adopted: HashSet<String> = world.harness.seqs[a]
        .events
        .iter()
        .filter_map(|e| match e {
            lb_zone_sdk::sequencer::Event::BlocksProcessed { channel_update, .. } => {
                Some(channel_update)
            }
            _ => None,
        })
        .flat_map(|update| update.adopted().iter())
        .filter_map(|entry| entry.inscription())
        .map(|info| payload_str(&info.payload))
        .collect();
    for payload in theirs {
        assert!(adopted.contains(payload), "SEQ_A never adopted {payload}");
    }
    world.finish();
}
scenario!(s27_foreign_sequencer_writes, foreign_sequencer_writes);

/// A deposit mined only in a losing fork block is not reported in
/// `BlocksProcessed::deposits`: it never reached the canonical chain.
#[tokio::test(start_paused = true)]
async fn deposit_in_losing_fork_is_not_reported() {
    let mut world = World::new(Network::Linear);
    world.coin_split(&[7]).await;
    let a = world.start("SEQ_A", passive()).await;
    let first = world.publish_included(a, "initial inscription").await;
    world.finalize_tx(first).await;

    let notes = world.take_user_notes(&[7]);
    let (hash, _, _) = world.deposit(&notes, "fork-only deposit").await;
    // A competing block on the tip's parent takes the mempool but stays a
    // dead end: same height as the tip, so it never becomes canonical.
    let parent = world.node().ancestor_of_tip(1);
    let slot = world.node().tip_slot().into_inner() + 1;
    let fork = world.node().mine_block(BlockSpec {
        parent: Some(parent),
        slot: Some(slot.into()),
        ..Default::default()
    });
    world.harness.settle().await;
    assert!(!world.node().is_canonical_block(fork), "fork must lose");
    assert!(
        !world.node().is_included(&hash),
        "deposit must not be canonical"
    );
    world.steps(3).await;
    world.harness.assert_layers();
}

/// Two sequencers withdraw against the same channel note; the loser's bundle
/// is pending with an input that no longer exists and must not wedge it.
async fn losing_withdraw_race_is_dropped(network: Network) {
    let mut world = World::new(network);
    world.coin_split(&[2]).await;
    let a = world.start("SEQ_A", passive()).await;
    world.config_finalized(a, &["SEQ_B"], 3, 0).await;
    let b = world.start("SEQ_B", passive()).await;
    let notes = world.take_user_notes(&[2]);
    let (hash, _, _) = world.deposit(&notes, "shared note").await;
    world.include(hash).await;
    world.finalize_tx(hash).await;

    let user_pk = world.user_pk();
    let mut hashes = Vec::new();
    for (index, payload) in [(a, "A takes 1"), (b, "B takes 1")] {
        let (result, _) = world
            .harness
            .seq(index)
            .handle()
            .publish_atomic_withdraw(
                inscription(payload),
                vec![WithdrawArg {
                    outputs: Outputs::new([Note::new(1, user_pk)]),
                }],
                WithdrawInputs::Auto,
            )
            .await
            .expect("withdraw accepted");
        hashes.push(result.inscription_id());
        world.harness.settle().await;
    }
    // Whichever lands spends the note; the other can never be mined.
    world.steps(12).await;
    let landed = hashes
        .iter()
        .filter(|h| world.node().is_included(h))
        .count();
    assert_eq!(
        landed,
        1,
        "exactly one withdraw lands:\n{}",
        world.diagnostics()
    );

    // The loser must still be able to post afterwards.
    let loser = if world.node().is_included(&hashes[0]) {
        b
    } else {
        a
    };
    let after = world.publish_retry(loser, "after the race").await;
    world.include_within(after, 60).await;
    world.settle_chain(120).await;
    world.finish();
}
scenario!(
    s28_losing_withdraw_race_is_dropped,
    losing_withdraw_race_is_dropped
);

/// Known SDK issue: a writer's inscription and the two-inscription tx chained
/// on it are mined, then reorged out while a price rise keeps the writer from
/// re-landing them, so both sit in the pending tail when the writer's config
/// lands. The config shed drops the inscription but not the custom tx chained
/// on it, which leaves the view without being reported orphaned.
#[tokio::test(start_paused = true)]
#[ignore = "known issue: the config shed drops a pending inscription but not the custom tx chained on it"]
async fn config_shed_strands_the_custom_tx_chained_on_a_shed_inscription() {
    let mut world = World::new(Network::Linear);
    let a = world.start("SEQ_A", passive()).await;
    world.config_finalized(a, &[FOREIGN], 1, 0).await;
    world.add_writer_with(
        FOREIGN,
        &[WriterOp::Inscribe, WriterOp::InscribeTwo, WriterOp::Config],
    );
    world.enable_foreign_traffic(2);
    let mined = world
        .step_until(12, |w| {
            w.writers[0].posted.len() == 2
                && w.writers[0]
                    .posted_hashes()
                    .all(|h| w.node().is_included(&h))
        })
        .await;
    assert!(
        mined,
        "the writer's inscription and custom tx were not mined:\n{}",
        world.diagnostics()
    );

    // Too dear to re-land: both stay in the pending tail of the new branch.
    world.node().set_gas_prices(GasPrices::new(2, 2));
    world.fork(3).await;
    world.enable_foreign_traffic(1);
    world.steps(2).await;
    let config = world.writers[0].posted_hashes().nth(2);
    assert!(
        config.is_some_and(|hash| world.node().is_included(&hash)),
        "the writer's config did not land:\n{}",
        world.diagnostics()
    );
    world.harness.assert_layers();
}

/// A withdraw spends the note of an unfinalized deposit; a fork drops the
/// deposit and the user spends the same input again, so the channel note
/// never exists. The bundle's lineage parent stays valid, so only its input
/// is dead: it must be shed, not resubmitted forever, and must not block
/// later publishes.
#[tokio::test(start_paused = true)]
#[ignore = "known issue: a pending withdraw whose channel input vanished with an orphaned deposit is resubmitted forever"]
async fn withdraw_on_orphaned_deposit_is_dropped() {
    let mut world = World::new(Network::Linear);
    world.coin_split(&[2]).await;
    let a = world.start("SEQ_A", passive()).await;
    let first = world.publish_included(a, "initial inscription").await;
    world.finalize_tx(first).await;

    let notes = world.take_user_notes(&[2]);
    let (deposit, _, _) = world.deposit(&notes, "first deposit").await;
    world.include(deposit).await;
    let unfinalized: Vec<u64> = world
        .wallet(a)
        .unfinalized
        .iter()
        .map(|n| n.value)
        .collect();
    assert_eq!(unfinalized, vec![2], "the deposit is in the wallet");

    let user_pk = world.user_pk();
    let (result, _) = world
        .harness
        .seq(a)
        .handle()
        .publish_atomic_withdraw(
            inscription("withdraw 1"),
            vec![WithdrawArg {
                outputs: Outputs::new([Note::new(1, user_pk)]),
            }],
            WithdrawInputs::Auto,
        )
        .await
        .expect("withdraw accepted");
    let withdraw = result.inscription_id();
    world.harness.settle().await;

    // Fork over the deposit's block; the fork spends the user's note in a
    // second deposit, so the first deposit can never return.
    let (second, _) = txs::deposit_tx(
        world.node(),
        world.channel_id(),
        &notes,
        "second deposit",
        user_pk,
    )
    .await
    .expect("second deposit funded");
    let parent = world.node().ancestor_of_tip(1);
    let tip_slot = world.node().tip_slot().into_inner();
    let fork = world.node().mine_block(BlockSpec {
        parent: Some(parent),
        slot: Some((tip_slot + 1).into()),
        txs: TxSelection::None,
        extra_txs: vec![second],
        ..BlockSpec::default()
    });
    world.harness.settle().await;
    let _ = world.node().mine_block(BlockSpec {
        parent: Some(fork),
        slot: Some((tip_slot + 2).into()),
        txs: TxSelection::Except(vec![deposit, withdraw]),
        ..BlockSpec::default()
    });
    world.harness.settle().await;
    world.steps(3).await;
    assert!(
        !world.node().is_included(&deposit),
        "the first deposit is dead"
    );

    // The bundle's input never exists again; the sequencer must let it go
    // and keep publishing.
    world.steps(40).await;
    assert!(!world.node().is_included(&withdraw));
    let after = world.publish_retry(a, "after the dead withdraw").await;
    world.include_within(after, 60).await;
    assert_eq!(
        world.pending_count(a),
        0,
        "SEQ_A still holds the dead withdraw:\n{}",
        world.diagnostics()
    );
    world.settle_chain(120).await;
    world.finish();
}
