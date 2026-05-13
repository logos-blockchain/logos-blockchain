use std::{collections::HashSet, num::NonZero, time::Duration};

use futures::{StreamExt as _, future::join_all};
use lb_chain_service::{ChainServiceMode, State};
use lb_common_http_client::{CommonHttpClient, Slot};
use lb_core::{
    block::genesis::GenesisBlock,
    mantle::{
        GenesisTx as _, MantleTx, Note, NoteId, Op, OpProof, Value,
        ledger::{Inputs, Outputs},
        ops::{
            channel::{ChannelId, deposit::DepositOp, withdraw::ChannelWithdrawOp},
            transfer::TransferOp,
        },
    },
    proofs::channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignature},
    sdp::{Locator, ServiceType},
};
use lb_http_api_common::bodies::{
    channel::ChannelDepositRequestBody,
    wallet::{
        balance::WalletBalanceResponseBody,
        sign::{WalletSignTxZkRequestBody, WalletSignTxZkResponseBody},
    },
};
use lb_key_management_system_service::keys::{Ed25519Key, ZkPublicKey, ZkSignature};
use lb_node::{SignedMantleTx, Transaction as _, config::RunConfig};
use lb_utils::math::NonNegativeRatio;
use lb_zone_sdk::{
    ZoneMessage,
    adapter::NodeHttpClient,
    indexer::ZoneIndexer,
    sequencer::{Event, SequencerConfig, SequencerHandle, ZoneSequencer},
    state::InscriptionInfo,
};

type Node = NodeHttpClient;
use lb_core::mantle::channel::{SlotTimeframe, SlotTimeout};
use logos_blockchain_tests::{
    common::{sync::wait_for_validators_mode_and_height, time::max_block_propagation_time},
    nodes::{Validator, create_validator_config},
    topology::configs::{
        GeneralConfig,
        consensus::{ProviderInfo, create_genesis_block_with_declarations},
        create_general_configs,
        deployment::e2e_deployment_settings_with_genesis_block,
    },
};
use rand::{Rng as _, thread_rng};
use tokio::time::{sleep, timeout};
use tracing::debug;

/// Initialize tracing subscriber once for all tests.
/// Controlled by `RUST_LOG` env var (e.g. `RUST_LOG=debug`).
fn init_tracing() {
    drop(
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            )
            .with_test_writer()
            .try_init(),
    );
}

fn channel_id_from_key(key: &Ed25519Key) -> ChannelId {
    ChannelId::from(key.public_key().to_bytes())
}

async fn wait_for_height(validator: &Validator, target_height: u64, duration: Duration) -> bool {
    timeout(duration, async {
        loop {
            let info = validator.consensus_info(false).await;
            if info.cryptarchia_info.height >= target_height {
                return;
            }
            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .is_ok()
}

async fn wait_for_lib_advance(
    validator: &Validator,
    initial_lib_slot: Slot,
    duration: Duration,
) -> bool {
    timeout(duration, async {
        loop {
            let info = validator.consensus_info(false).await;
            if info.cryptarchia_info.lib_slot > initial_lib_slot {
                return;
            }
            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .is_ok()
}

#[tokio::test]
async fn test_sequencer_publish_and_indexer_read() {
    init_tracing();
    let validators = spawn_validators(
        Some("test_sequencer_publish_and_indexer_read"),
        1,
        |mut config| {
            config.deployment.time.slot_duration = Duration::from_secs(1);
            config
                .user
                .cryptarchia
                .service
                .bootstrap
                .prolonged_bootstrap_period = Duration::ZERO;
            config.deployment.cryptarchia.security_param = NonZero::new(3).unwrap();
            config.deployment.cryptarchia.slot_activation_coeff =
                NonNegativeRatio::new(1, 2.try_into().unwrap());
            config
        },
        3,
    )
    .await;
    let validator = &validators[0];
    let node_url = validator.url();

    let signing_key = keygen();
    let admin_pk = signing_key.public_key();
    let channel_id = channel_id_from_key(&signing_key);

    let sequencer_config = SequencerConfig {
        resubmit_interval: Duration::from_secs(3),
        ..SequencerConfig::default()
    };
    let (sequencer, mut handle) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url.clone()),
        sequencer_config,
        None,
    );

    let (drive_task, _rx) = spawn_drive(sequencer);

    let test_data: Vec<Vec<u8>> = vec![
        b"Hello, Zone!".to_vec(),
        b"Second message".to_vec(),
        b"Third message".to_vec(),
    ];

    publish_all(&mut handle, &test_data).await;

    let indexer = ZoneIndexer::new(
        channel_id,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url),
    );

    wait_for_indexer_ordered(&indexer, &test_data, Duration::from_mins(6)).await;

    // Test channel_config: update channel's accredited keys
    let second_pk = keygen().public_key();
    let (_result, finalized) = handle
        .channel_config(vec![admin_pk, second_pk], 0.into(), 0.into(), 1, 1)
        .await
        .expect("channel_config should succeed");
    timeout(Duration::from_mins(6), finalized)
        .await
        .expect("Timeout waiting for channel_config to finalize")
        .expect("channel_config finalization failed");

    drive_task.abort();
}

#[tokio::test]
async fn test_sequencer_checkpoint_resume() {
    init_tracing();
    let validators = spawn_validators(
        Some("test_sequencer_checkpoint_resume"),
        1,
        |mut config| {
            config.deployment.time.slot_duration = Duration::from_secs(1);
            config
                .user
                .cryptarchia
                .service
                .bootstrap
                .prolonged_bootstrap_period = Duration::ZERO;
            config.deployment.cryptarchia.security_param = NonZero::new(3).unwrap();
            config.deployment.cryptarchia.slot_activation_coeff =
                NonNegativeRatio::new(1, 2.try_into().unwrap());
            config
        },
        3,
    )
    .await;
    let validator = &validators[0];
    let node_url = validator.url();

    let signing_key = keygen();
    let channel_id = channel_id_from_key(&signing_key);

    let sequencer_config = SequencerConfig {
        resubmit_interval: Duration::from_secs(3),
        ..SequencerConfig::default()
    };

    // Phase 1: Publish and capture checkpoint via Published events
    let (sequencer, mut handle) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key.clone(),
        NodeHttpClient::new(CommonHttpClient::new(None), node_url.clone()),
        sequencer_config.clone(),
        None,
    );
    let (drive_task, mut rx) = spawn_drive(sequencer);
    handle.wait_ready().await;

    let test_data_phase1: Vec<Vec<u8>> = vec![b"Message 1".to_vec(), b"Message 2".to_vec()];
    for data in &test_data_phase1 {
        handle
            .publish_message(data.clone())
            .await
            .expect("publish should succeed");
    }

    let mut checkpoint = None;
    let mut published_count = 0;
    while let Some(event) = rx.recv().await {
        if let Event::Published { checkpoint: cp, .. } = event {
            checkpoint = Some(cp);
            published_count += 1;
            if published_count >= test_data_phase1.len() {
                break;
            }
        }
    }
    let checkpoint = checkpoint.expect("should receive Published event");

    drive_task.abort();
    drop(handle);

    // Phase 2: Resume with checkpoint and publish more
    let (sequencer, mut handle) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url.clone()),
        sequencer_config,
        Some(checkpoint),
    );
    let (drive_task, _rx) = spawn_drive(sequencer);

    let test_data_phase2: Vec<Vec<u8>> = vec![b"Message 3".to_vec(), b"Message 4".to_vec()];
    publish_all(&mut handle, &test_data_phase2).await;

    let indexer = ZoneIndexer::new(
        channel_id,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url),
    );
    let all_test_data: Vec<Vec<u8>> = test_data_phase1
        .into_iter()
        .chain(test_data_phase2)
        .collect();
    wait_for_indexer_ordered(&indexer, &all_test_data, Duration::from_mins(6)).await;

    drive_task.abort();
}

/// Generate a random Ed25519 signing key.
fn keygen() -> Ed25519Key {
    let mut key_bytes = [0u8; 32];
    thread_rng().fill(&mut key_bytes);
    Ed25519Key::from_bytes(&key_bytes)
}

/// Drive the sequencer event loop in a background task, forwarding events.
fn spawn_drive(
    mut sequencer: ZoneSequencer<Node>,
) -> (
    tokio::task::JoinHandle<()>,
    tokio::sync::mpsc::Receiver<Event>,
) {
    let (tx, rx) = tokio::sync::mpsc::channel(256);
    let handle = tokio::spawn(async move {
        loop {
            if let Some(event) = sequencer.next_event().await {
                drop(tx.send(event).await);
            }
        }
    });
    (handle, rx)
}

/// Drive the sequencer with republish-on-conflict behavior.
///
/// Maintains a local pending set keyed by `msg_id`, populated on
/// `Event::Published` and pruned on finalize/orphan. On each `ChannelUpdate`,
/// republishes any `orphaned` entry whose `msg_id` is in local pending —
/// this is the only correct dedup when payloads are not unique (the SDK's
/// `pending` field happens to work for unique payloads but breaks when
/// multiple in-flight publishes share the same bytes).
///
/// If `finalized_tx` is provided, every finalized inscription seen via
/// `Event::TxsFinalized` or `Event::FinalizedInscriptions` is forwarded so
/// the test can deterministically wait for finality of expected payloads
/// instead of using a fixed sleep.
fn spawn_drive_republish(
    mut sequencer: ZoneSequencer<Node>,
    handle: SequencerHandle<Node>,
    finalized_tx: Option<tokio::sync::mpsc::UnboundedSender<InscriptionInfo>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut local_pending: HashSet<lb_core::mantle::ops::channel::MsgId> = HashSet::new();
        loop {
            match sequencer.next_event().await {
                Some(Event::Published { info, .. }) => {
                    local_pending.insert(info.this_msg);
                }
                Some(
                    Event::TxsFinalized { inscriptions, .. }
                    | Event::FinalizedInscriptions { inscriptions },
                ) => {
                    for ins in &inscriptions {
                        local_pending.remove(&ins.this_msg);
                    }
                    if let Some(tx) = finalized_tx.as_ref() {
                        for ins in inscriptions {
                            drop(tx.send(ins));
                        }
                    }
                }
                Some(Event::ChannelUpdate { orphaned, .. }) => {
                    for info in &orphaned {
                        if local_pending.remove(&info.this_msg) {
                            debug!(
                                "Re-publishing: {:?}",
                                String::from_utf8_lossy(&info.payload)
                            );
                            if let Err(e) = handle.publish_message(info.payload.clone()).await {
                                debug!("Failed to re-publish: {e}");
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    })
}

/// Helper: wait for readiness then publish all payloads.
async fn publish_all(handle: &mut SequencerHandle<Node>, payloads: &[Vec<u8>]) {
    handle.wait_ready().await;
    for data in payloads {
        handle
            .publish_message(data.clone())
            .await
            .expect("publish should succeed after wait_ready");
    }
}

/// Wait for all expected payloads to appear in the indexer (any order).
async fn wait_for_indexer_unordered(
    indexer: &ZoneIndexer<Node>,
    expected: &HashSet<Vec<u8>>,
    timeout_duration: Duration,
) {
    let mut seen: HashSet<Vec<u8>> = HashSet::new();
    let mut last_zone_block = None;
    let start = std::time::Instant::now();

    loop {
        assert!(
            start.elapsed() <= timeout_duration,
            "Timeout waiting for indexer to return all messages"
        );

        let stream = indexer
            .next_messages(last_zone_block)
            .await
            .expect("next_messages should succeed");
        futures::pin_mut!(stream);

        while let Some((msg, slot)) = stream.next().await {
            if let ZoneMessage::Block(block) = msg {
                if expected.contains(&block.data) {
                    assert!(
                        seen.insert(block.data.clone()),
                        "Duplicate inscription on chain: {:?}",
                        String::from_utf8_lossy(&block.data)
                    );
                    debug!("Found payload: {}", String::from_utf8_lossy(&block.data));
                }
                last_zone_block = Some((block.id, slot));
            }
        }

        if seen == *expected {
            break;
        }

        sleep(Duration::from_millis(500)).await;
    }
}

/// Wait until at least `expected_count` distinct finalizations whose payload
/// is in `expected` have been observed via `finalized_rx`. The receiver is
/// fed by sequencer handlers forwarding `Event::TxsFinalized` /
/// `Event::FinalizedInscriptions` inscriptions; multiple sequencers can share
/// the same sender so deduplication by `tx_hash` is needed (each finalization
/// is reported once per sequencer).
///
/// Returns the set of distinct `tx_hash`es observed as finalized.
async fn wait_for_finalized(
    finalized_rx: &mut tokio::sync::mpsc::UnboundedReceiver<InscriptionInfo>,
    expected: &HashSet<Vec<u8>>,
    expected_count: usize,
    timeout_duration: Duration,
) -> HashSet<lb_core::mantle::tx::TxHash> {
    let start = std::time::Instant::now();
    let mut seen_tx_hashes: HashSet<lb_core::mantle::tx::TxHash> = HashSet::new();
    let mut matched_count: usize = 0;

    while matched_count < expected_count {
        assert!(
            start.elapsed() <= timeout_duration,
            "Timeout: only {matched_count}/{expected_count} expected finalizations seen"
        );

        match timeout(Duration::from_secs(5), finalized_rx.recv()).await {
            Ok(Some(ins)) => {
                if expected.contains(&ins.payload) && seen_tx_hashes.insert(ins.tx_hash) {
                    matched_count += 1;
                }
            }
            Ok(None) => break,
            Err(_) => {
                // 5s heartbeat — re-check timeout.
            }
        }
    }

    seen_tx_hashes
}

/// Wait for expected payloads to appear in the indexer in exact order.
async fn wait_for_indexer_ordered(
    indexer: &ZoneIndexer<Node>,
    expected: &[Vec<u8>],
    timeout_duration: Duration,
) {
    let mut received: Vec<Vec<u8>> = Vec::new();
    let expected_set: HashSet<&Vec<u8>> = expected.iter().collect();
    let mut last_zone_block = None;
    let start = std::time::Instant::now();

    loop {
        assert!(
            start.elapsed() <= timeout_duration,
            "Timeout waiting for indexer to return all messages in order"
        );

        let stream = indexer
            .next_messages(last_zone_block)
            .await
            .expect("next_messages should succeed");
        futures::pin_mut!(stream);

        while let Some((msg, slot)) = stream.next().await {
            if let ZoneMessage::Block(block) = msg {
                if expected_set.contains(&block.data) {
                    received.push(block.data.clone());
                    debug!(
                        "Found payload ({}/{}): {}",
                        received.len(),
                        expected.len(),
                        String::from_utf8_lossy(&block.data)
                    );
                }
                last_zone_block = Some((block.id, slot));
            }
        }

        if received.len() >= expected.len() {
            break;
        }

        sleep(Duration::from_millis(500)).await;
    }

    assert_eq!(received, expected, "Messages should match expected order");
}

/// Helper: tag a message with a random ID for reorg deduplication.
fn tag_payload(msg: &str) -> Vec<u8> {
    format!("{:016x}:{msg}", rand::random::<u64>()).into_bytes()
}

/// Spawn `n` validators with the standard fast-block test config and wait
/// for the chain to produce its first block. Returns the validators and the
/// URL of the first one (where sequencers + indexer connect).
async fn spawn_competing_validators(n: usize) -> (Vec<Validator>, reqwest::Url) {
    let (configs, genesis_block) = create_general_configs(n, None);
    let deployment_settings = e2e_deployment_settings_with_genesis_block(&genesis_block);
    let configs: Vec<_> = configs
        .into_iter()
        .map(|c| {
            let mut config = create_validator_config(c, deployment_settings.clone());
            config.deployment.time.slot_duration = Duration::from_secs(1);
            config
                .user
                .cryptarchia
                .service
                .bootstrap
                .prolonged_bootstrap_period = Duration::ZERO;
            config.deployment.cryptarchia.security_param = NonZero::new(3).unwrap();
            config.deployment.cryptarchia.slot_activation_coeff =
                NonNegativeRatio::new(1, 2.try_into().unwrap());
            config
        })
        .collect();

    let validators: Vec<Validator> = join_all(configs.into_iter().map(Validator::spawn))
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("Failed to spawn validators");

    assert!(
        wait_for_height(&validators[0], 1, Duration::from_mins(2)).await,
        "Chain should produce the first block"
    );
    let node_url = validators[0].url();
    (validators, node_url)
}

/// Bootstrap the channel by submitting `channel_config` from a transient
/// sequencer using `admin_key`. Waits for finalization, then drops the
/// sequencer.
async fn authorize_keys(
    channel_id: ChannelId,
    admin_key: Ed25519Key,
    keys: Vec<lb_core::mantle::ops::channel::Ed25519PublicKey>,
    posting_timeframe: SlotTimeframe,
    posting_timeout: SlotTimeout,
    node_url: reqwest::Url,
    sequencer_config: SequencerConfig,
) {
    let (sequencer, mut handle) = ZoneSequencer::init_with_config(
        channel_id,
        admin_key,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url),
        sequencer_config,
        None,
    );
    let (poll, _rx) = spawn_drive(sequencer);
    handle.wait_ready().await;
    let (_result, finalized) = handle
        .channel_config(keys, posting_timeframe, posting_timeout, 1, 1)
        .await
        .expect("channel_config should succeed");
    timeout(Duration::from_mins(6), finalized)
        .await
        .expect("Timeout waiting for channel_config to finalize")
        .expect("channel_config finalization failed");
    poll.abort();
}

/// Convenience wrapper around `ZoneSequencer::init_with_config` for tests that
/// always start fresh (no checkpoint) and connect via the standard HTTP client.
fn init_sequencer(
    channel_id: ChannelId,
    signing_key: Ed25519Key,
    node_url: reqwest::Url,
    config: SequencerConfig,
) -> (ZoneSequencer<Node>, SequencerHandle<Node>) {
    ZoneSequencer::init_with_config(
        channel_id,
        signing_key,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url),
        config,
        None,
    )
}

/// Drive each `(handle, payloads)` pair in parallel: every handle publishes
/// its payloads in order, but the handles run concurrently so their messages
/// interleave on chain.
async fn publish_concurrently(jobs: Vec<(SequencerHandle<Node>, Vec<Vec<u8>>)>) {
    join_all(jobs.into_iter().map(async |(handle, data)| {
        for d in data {
            handle.publish_message(d).await.expect("publish failed");
        }
    }))
    .await;
}

/// Scan the indexer end-to-end for all on-chain payloads matching `expected`.
/// Used after settlement to detect duplicates the test should have prevented.
async fn scan_indexer_for_payloads(
    indexer: &ZoneIndexer<Node>,
    expected: &HashSet<Vec<u8>>,
) -> Vec<Vec<u8>> {
    let mut all_payloads: Vec<Vec<u8>> = Vec::new();
    let mut last_zone_block = None;
    loop {
        let stream = indexer
            .next_messages(last_zone_block)
            .await
            .expect("next_messages should succeed");
        futures::pin_mut!(stream);
        let mut got_any = false;
        while let Some((msg, slot)) = stream.next().await {
            got_any = true;
            if let ZoneMessage::Block(block) = msg {
                if expected.contains(&block.data) {
                    debug!(
                        "Post-settlement scan: {:?} id={:?} slot={slot:?}",
                        String::from_utf8_lossy(&block.data),
                        block.id,
                    );
                    all_payloads.push(block.data.clone());
                }
                last_zone_block = Some((block.id, slot));
            }
        }
        if !got_any {
            break;
        }
    }
    all_payloads
}

/// Two sequencer instances sharing a single signing key, running sequentially.
/// This exercises the "horizontally scaled sequencer" deployment: each
/// instance has its own outbox, but they sign with the same key. Inscription
/// uniqueness comes from the payload, not the signer.
#[tokio::test]
async fn test_sequential_multi_sequencer() {
    init_tracing();
    let (_validators, node_url) = spawn_competing_validators(2).await;

    let sequencer_config = SequencerConfig {
        resubmit_interval: Duration::from_secs(3),
        ..SequencerConfig::default()
    };

    // Single shared signing key — both sequencer instances sign with it.
    let signing_key = keygen();
    let channel_id = channel_id_from_key(&signing_key);

    // --- Phase 1: SeqA (instance #1) publishes a1, a2, a3 ---
    let (sequencer_a, mut handle_a) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key.clone(),
        NodeHttpClient::new(CommonHttpClient::new(None), node_url.clone()),
        sequencer_config.clone(),
        None,
    );
    let (poll_a, _rx) = spawn_drive(sequencer_a);

    let phase1_data: Vec<Vec<u8>> = vec![tag_payload("a1"), tag_payload("a2"), tag_payload("a3")];
    publish_all(&mut handle_a, &phase1_data).await;

    let indexer = ZoneIndexer::new(
        channel_id,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url.clone()),
    );
    let expected_phase1: HashSet<Vec<u8>> = phase1_data.iter().cloned().collect();
    wait_for_indexer_unordered(&indexer, &expected_phase1, Duration::from_mins(6)).await;

    // Stop SeqA
    poll_a.abort();
    drop(handle_a);

    // --- Phase 2: SeqB (instance #2, same key) publishes b1, b2, b3 ---
    let (sequencer_b, mut handle_b) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key.clone(),
        NodeHttpClient::new(CommonHttpClient::new(None), node_url.clone()),
        sequencer_config.clone(),
        None, // Fresh start — SeqB discovers channel tip from chain
    );
    let (poll_b, _rx) = spawn_drive(sequencer_b);

    let phase2_data: Vec<Vec<u8>> = vec![tag_payload("b1"), tag_payload("b2"), tag_payload("b3")];
    publish_all(&mut handle_b, &phase2_data).await;

    let mut expected_phase2 = expected_phase1.clone();
    expected_phase2.extend(phase2_data.iter().cloned());
    wait_for_indexer_unordered(&indexer, &expected_phase2, Duration::from_mins(6)).await;

    // Stop SeqB
    poll_b.abort();
    drop(handle_b);

    // --- Phase 3: SeqA resumes (instance #3, same key) and publishes a4, a5, a6
    // ---
    let (sequencer_a, mut handle_a) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url.clone()),
        sequencer_config,
        None, // Fresh start — discovers current channel tip
    );
    let (poll_a, _rx) = spawn_drive(sequencer_a);

    let phase3_data: Vec<Vec<u8>> = vec![tag_payload("a4"), tag_payload("a5"), tag_payload("a6")];
    publish_all(&mut handle_a, &phase3_data).await;

    // Verify all 9 inscriptions on chain in expected order:
    // a1, a2, a3 (phase1), b1, b2, b3 (phase2), a4, a5, a6 (phase3)
    let expected_order: Vec<Vec<u8>> = phase1_data
        .into_iter()
        .chain(phase2_data)
        .chain(phase3_data)
        .collect();
    wait_for_indexer_ordered(&indexer, &expected_order, Duration::from_mins(6)).await;

    // Clean up
    poll_a.abort();
}

const CONCURRENT_TEST_N_PER_SEQUENCER: usize = 20;

#[tokio::test]
async fn test_concurrent_multi_sequencer() {
    init_tracing();
    // Three sequencer instances sharing a single signing key, publishing
    // concurrently — the horizontally-scaled-sequencer deployment. Each
    // instance has its own outbox; uniqueness comes from distinct payloads.
    // Validates that ownership classification (own writes vs others') stays
    // correct under shared-key contention.
    let (_validators, node_url) = spawn_competing_validators(2).await;

    let sequencer_config = SequencerConfig {
        resubmit_interval: Duration::from_secs(30),
        ..SequencerConfig::default()
    };

    // Single shared signing key for all three instances. The channel is
    // implicitly created by the first inscription, so no separate
    // authorization step is needed.
    let signing_key = keygen();
    let channel_id = channel_id_from_key(&signing_key);

    // Prepare payloads before starting sequencers
    let data_a: Vec<Vec<u8>> = (1..=CONCURRENT_TEST_N_PER_SEQUENCER)
        .map(|i| tag_payload(&format!("a{i}")))
        .collect();
    let data_b: Vec<Vec<u8>> = (1..=CONCURRENT_TEST_N_PER_SEQUENCER)
        .map(|i| tag_payload(&format!("b{i}")))
        .collect();
    let data_c: Vec<Vec<u8>> = (1..=CONCURRENT_TEST_N_PER_SEQUENCER)
        .map(|i| tag_payload(&format!("c{i}")))
        .collect();
    let total = CONCURRENT_TEST_N_PER_SEQUENCER * 3;

    // --- Phase 2: Start three sequencer instances sharing the signing key ---
    debug!("Phase 2: Starting 3 shared-key sequencer instances concurrently");
    let (seq_a, mut handle_a) = init_sequencer(
        channel_id,
        signing_key.clone(),
        node_url.clone(),
        sequencer_config.clone(),
    );
    let (seq_b, mut handle_b) = init_sequencer(
        channel_id,
        signing_key.clone(),
        node_url.clone(),
        sequencer_config.clone(),
    );
    let (seq_c, mut handle_c) =
        init_sequencer(channel_id, signing_key, node_url.clone(), sequencer_config);

    // Aggregate finalization reports from all sequencers into one channel —
    // dedup by tx_hash since each finalization is reported once per sequencer.
    let (finalized_tx, mut finalized_rx) = tokio::sync::mpsc::unbounded_channel();
    let poll_a = spawn_drive_republish(seq_a, handle_a.clone(), Some(finalized_tx.clone()));
    let poll_b = spawn_drive_republish(seq_b, handle_b.clone(), Some(finalized_tx.clone()));
    let poll_c = spawn_drive_republish(seq_c, handle_c.clone(), Some(finalized_tx));

    handle_a.wait_ready().await;
    handle_b.wait_ready().await;
    handle_c.wait_ready().await;
    debug!("Phase 2: All 3 sequencers ready");

    // Phase 3: Publish initial inscriptions concurrently.
    debug!("Phase 3: Publishing {total} inscriptions concurrently");
    publish_concurrently(vec![
        (handle_a, data_a.clone()),
        (handle_b, data_b.clone()),
        (handle_c, data_c.clone()),
    ])
    .await;

    // Phase 4: Wait deterministically for all expected payloads to be
    // finalized (i.e., below LIB and immutable). Once finalized, no more
    // changes possible to those slots.
    debug!("Phase 4: Waiting for all {total} expected payloads to finalize");
    let indexer = ZoneIndexer::new(
        channel_id,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url),
    );
    let expected_all: HashSet<Vec<u8>> = data_a
        .iter()
        .chain(&data_b)
        .chain(&data_c)
        .cloned()
        .collect();
    assert_eq!(expected_all.len(), total);

    wait_for_finalized(
        &mut finalized_rx,
        &expected_all,
        total,
        Duration::from_mins(20),
    )
    .await;

    let all_payloads = scan_indexer_for_payloads(&indexer, &expected_all).await;

    let unique: HashSet<&Vec<u8>> = all_payloads.iter().collect();
    assert_eq!(
        unique.len(),
        all_payloads.len(),
        "Duplicate inscriptions detected on chain: expected {} unique, got {} total",
        unique.len(),
        all_payloads.len(),
    );
    assert_eq!(
        unique.len(),
        total,
        "Expected exactly {total} inscriptions on chain"
    );

    // Clean up
    poll_a.abort();
    poll_b.abort();
    poll_c.abort();
}

/// Spawn a sequencer with a "smallest wins" conflict resolution policy.
///
/// When a competing inscription takes our parent:
/// - If the adopted payload is lexicographically smaller → drop ours (correct
///   order, the smaller one should come first).
/// - If ours is smaller → re-publish (we should have gone first).
///
/// The result is that the on-chain sequence is always sorted.
type DiscardedSet = std::sync::Arc<tokio::sync::Mutex<HashSet<Vec<u8>>>>;

fn spawn_sequencer_sorted_policy(
    mut sequencer: ZoneSequencer<Node>,
    handle: SequencerHandle<Node>,
    discarded: DiscardedSet,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut max_seen_on_chain: Option<Vec<u8>> = None;

        loop {
            let event = sequencer.next_event().await;
            if let Some(Event::Published { info, .. }) = event {
                if max_seen_on_chain.as_ref().is_none_or(|m| info.payload > *m) {
                    max_seen_on_chain = Some(info.payload);
                }
                continue;
            }
            let Some(Event::ChannelUpdate { orphaned, adopted }) = event else {
                continue;
            };

            for a in &adopted {
                discarded.lock().await.remove(&a.payload);
                if max_seen_on_chain.as_ref().is_none_or(|m| a.payload > *m) {
                    max_seen_on_chain = Some(a.payload.clone());
                }
            }

            for info in &orphaned {
                if discarded.lock().await.contains(&info.payload) {
                    continue;
                }
                let larger_or_equal = max_seen_on_chain
                    .as_ref()
                    .is_some_and(|m| info.payload >= *m);
                if larger_or_equal {
                    debug!(
                        "Sorted policy: re-publishing {:?} (>= max {:?})",
                        String::from_utf8_lossy(&info.payload),
                        max_seen_on_chain
                            .as_ref()
                            .map(|m| String::from_utf8_lossy(m).to_string()),
                    );
                    if let Err(e) = handle.publish_message(info.payload.clone()).await {
                        debug!("Failed to re-publish: {e}");
                    }
                } else {
                    debug!(
                        "Sorted policy: dropping {:?} (< max {:?})",
                        String::from_utf8_lossy(&info.payload),
                        max_seen_on_chain
                            .as_ref()
                            .map(|m| String::from_utf8_lossy(m).to_string()),
                    );
                    discarded.lock().await.insert(info.payload.clone());
                }
            }
        }
    })
}

/// Poll the indexer until `total - discarded.len()` of `expected` have settled
/// on chain. Returns the on-chain payloads in order of arrival.
async fn wait_until_settled(
    indexer: &ZoneIndexer<Node>,
    expected: &HashSet<Vec<u8>>,
    discarded: &DiscardedSet,
    total: usize,
) -> Vec<Vec<u8>> {
    let mut on_chain: Vec<Vec<u8>> = Vec::new();
    let mut last_zone_block = None;
    let start = std::time::Instant::now();
    loop {
        assert!(
            start.elapsed() <= Duration::from_mins(10),
            "Timeout waiting for inscriptions to finalize"
        );
        let expected_count = total - discarded.lock().await.len();
        if on_chain.len() >= expected_count && expected_count > 0 {
            break;
        }
        let stream = indexer
            .next_messages(last_zone_block)
            .await
            .expect("next_messages should succeed");
        futures::pin_mut!(stream);
        while let Some((msg, slot)) = stream.next().await {
            if let ZoneMessage::Block(block) = msg {
                if expected.contains(&block.data) && !on_chain.contains(&block.data) {
                    on_chain.push(block.data.clone());
                    debug!(
                        "Indexer found: {:?} ({}/{})",
                        String::from_utf8_lossy(&block.data),
                        on_chain.len(),
                        expected_count,
                    );
                }
                last_zone_block = Some((block.id, slot));
            }
        }
        sleep(Duration::from_millis(500)).await;
    }
    on_chain
}

/// Verify the sorted-policy invariants: no duplicates, ascending order,
/// `on_chain ∩ discarded == ∅`, and `on_chain ∪ discarded` covers all `total`
/// published payloads.
fn assert_sorted_outcome(on_chain: &[Vec<u8>], discarded: &HashSet<Vec<u8>>, total: usize) {
    let pretty = |bs: &[Vec<u8>]| {
        bs.iter()
            .map(|p| String::from_utf8_lossy(p).to_string())
            .collect::<Vec<_>>()
    };
    debug!("On-chain payloads: {:?}", pretty(on_chain));

    let unique: HashSet<&Vec<u8>> = on_chain.iter().collect();
    assert_eq!(
        unique.len(),
        on_chain.len(),
        "Duplicate inscriptions detected on chain"
    );
    assert!(
        on_chain.windows(2).all(|w| w[0] <= w[1]),
        "On-chain payloads must be sorted, got: {:?}",
        pretty(on_chain)
    );
    assert!(
        !on_chain.is_empty(),
        "At least some payloads should be on chain"
    );

    debug!(
        "{} on chain + {} discarded = {} (of {} published)",
        on_chain.len(),
        discarded.len(),
        on_chain.len() + discarded.len(),
        total
    );

    let on_chain_set: HashSet<Vec<u8>> = on_chain.iter().cloned().collect();
    let overlap: Vec<_> = on_chain_set.intersection(discarded).cloned().collect();
    assert!(
        overlap.is_empty(),
        "Payload both on chain and discarded: {:?}",
        pretty(&overlap)
    );
    assert_eq!(
        on_chain.len() + discarded.len(),
        total,
        "on_chain + discarded must equal total published"
    );
}

#[tokio::test]
async fn test_sorted_conflict_resolution() {
    init_tracing();
    // Two sequencers publish interleaved sorted payloads concurrently.
    // Custom policy: "smallest wins" — when a conflict occurs, the
    // lexicographically smaller payload keeps its position; the larger
    // one is dropped. The on-chain result must be sorted.
    let (_validators, node_url) = spawn_competing_validators(2).await;

    let sequencer_config = SequencerConfig {
        resubmit_interval: Duration::from_secs(3),
        ..SequencerConfig::default()
    };

    let signing_key_a = keygen();
    let admin_pk = signing_key_a.public_key();
    let channel_id = channel_id_from_key(&signing_key_a);
    let signing_key_b = keygen();
    let seq_b_pk = signing_key_b.public_key();

    // Phase 1: SeqA creates channel and authorizes SeqB.
    // posting_timeframe=10 → each sequencer has ~10s windows in turn so both
    // can land their messages within the test.
    authorize_keys(
        channel_id,
        signing_key_a.clone(),
        vec![admin_pk, seq_b_pk],
        10.into(),
        0.into(),
        node_url.clone(),
        sequencer_config.clone(),
    )
    .await;

    // Phase 2: Both sequencers publish interleaved sorted payloads.
    // SeqA: "aa", "cc", "ee", "gg", "ii"; SeqB: "bb", "dd", "ff", "hh", "jj"
    let data_a: Vec<Vec<u8>> = ["aa", "cc", "ee", "gg", "ii"]
        .iter()
        .map(|s| s.as_bytes().to_vec())
        .collect();
    let data_b: Vec<Vec<u8>> = ["bb", "dd", "ff", "hh", "jj"]
        .iter()
        .map(|s| s.as_bytes().to_vec())
        .collect();
    let total = data_a.len() + data_b.len();

    let (seq_a, mut handle_a) = init_sequencer(
        channel_id,
        signing_key_a,
        node_url.clone(),
        sequencer_config.clone(),
    );
    let (seq_b, mut handle_b) = init_sequencer(
        channel_id,
        signing_key_b,
        node_url.clone(),
        sequencer_config,
    );

    let discarded: DiscardedSet = std::sync::Arc::new(tokio::sync::Mutex::new(HashSet::new()));
    let poll_a =
        spawn_sequencer_sorted_policy(seq_a, handle_a.clone(), DiscardedSet::clone(&discarded));
    let poll_b =
        spawn_sequencer_sorted_policy(seq_b, handle_b.clone(), DiscardedSet::clone(&discarded));

    handle_a.wait_ready().await;
    handle_b.wait_ready().await;
    publish_concurrently(vec![(handle_a, data_a.clone()), (handle_b, data_b.clone())]).await;

    // Phase 3: Poll indexer until settled, then assert invariants.
    let indexer = ZoneIndexer::new(
        channel_id,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url),
    );
    let all_payloads: HashSet<Vec<u8>> = data_a.iter().chain(&data_b).cloned().collect();
    let on_chain = wait_until_settled(&indexer, &all_payloads, &discarded, total).await;
    let discarded_snapshot = discarded.lock().await.clone();
    assert_sorted_outcome(&on_chain, &discarded_snapshot, total);

    poll_a.abort();
    poll_b.abort();
}

/// Encode a balance-affecting transaction as `"<tx_uuid>:<account>:<delta>"`
/// bytes.
fn make_balance_tx(uuid: &str, account: &str, delta: i64) -> Vec<u8> {
    format!("{uuid}:{account}:{delta}").into_bytes()
}

/// Parse a balance-affecting transaction from the inscription payload.
fn parse_balance_tx(bytes: &[u8]) -> Option<(String, String, i64)> {
    let s = std::str::from_utf8(bytes).ok()?;
    let parts: Vec<&str> = s.splitn(3, ':').collect();
    if parts.len() != 3 {
        return None;
    }
    let delta = parts[2].parse::<i64>().ok()?;
    Some((parts[0].to_owned(), parts[1].to_owned(), delta))
}

/// Spawn a sequencer that conditions republish decisions on the current
/// canonical balance state per account.
///
/// Each inscription has the form `"<tx_uuid>:<account>:<delta>"`. The
/// sequencer maintains per-account balances from the SDK's
/// Published/adopted/orphaned events and only republishes when the resulting
/// balance for that account after applying its own delta would stay
/// non-negative.
fn spawn_balance_aware(
    mut sequencer: ZoneSequencer<Node>,
    handle: SequencerHandle<Node>,
    initial_balances: std::collections::HashMap<String, i64>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // applied[account][uuid] = delta
        let mut applied: std::collections::HashMap<String, std::collections::HashMap<String, i64>> =
            std::collections::HashMap::new();

        loop {
            let event = sequencer.next_event().await;
            if let Some(Event::Published { info, .. }) = event {
                // Optimistic apply: we just published this.
                if let Some((uuid, account, delta)) = parse_balance_tx(&info.payload) {
                    applied.entry(account).or_default().insert(uuid, delta);
                }
                continue;
            }
            let Some(Event::ChannelUpdate { orphaned, adopted }) = event else {
                continue;
            };

            for o in &orphaned {
                if let Some((uuid, account, _)) = parse_balance_tx(&o.payload)
                    && let Some(account_map) = applied.get_mut(&account)
                {
                    account_map.remove(&uuid);
                }
            }
            for a in &adopted {
                if let Some((uuid, account, delta)) = parse_balance_tx(&a.payload) {
                    applied.entry(account).or_default().insert(uuid, delta);
                }
            }

            for info in &orphaned {
                let Some((uuid, account, delta)) = parse_balance_tx(&info.payload) else {
                    continue;
                };
                let account_map = applied.entry(account.clone()).or_default();
                if account_map.contains_key(&uuid) {
                    continue;
                }
                let initial = initial_balances.get(&account).copied().unwrap_or(0);
                let current_applied: i64 = account_map.values().sum();
                let balance = initial + current_applied;
                if balance + delta >= 0 {
                    debug!("Balance OK ({account} = {balance} + {delta}), re-publishing {uuid}",);
                    if let Err(e) = handle.publish_message(info.payload.clone()).await {
                        debug!("Failed to re-publish: {e}");
                    }
                } else {
                    debug!(
                        "Balance insufficient ({account} = {balance} + {delta} < 0), dropping {uuid}",
                    );
                }
            }
        }
    })
}

/// Three sequencers each publish burns against three accounts (alice, bob,
/// charlie), each starting with balance 10. Republish policy: only republish
/// if the burn would still keep the affected account's balance non-negative.
///
/// Per-sequencer payloads (payload format `"<tx_uuid>:<account>:<delta>"`):
///   `SeqA`: burn 6 alice, burn 3 bob, burn 2 charlie
///   `SeqB`: burn 5 alice, burn 4 bob, burn 8 charlie
///   `SeqC`: burn 4 alice, burn 7 bob, burn 1 charlie
///
/// Per-account totals if all 9 burns landed:
///   alice: -15  (initial 10 → would go to -5)
///   bob:   -14  (initial 10 → would go to -4)
///   charlie: -11 (initial 10 → would go to -1)
///
/// So some burns must be dropped per account. Each sequencer's policy
/// computes per-account balance from its local view and decides republish vs
/// drop accordingly.
///
/// Test invariant: every account's final balance computed from on-chain
/// inscriptions must be non-negative. If state mirror drifts, a sequencer can
/// republish when it shouldn't and an account's balance goes negative —
/// this is the failure case the test catches.
#[expect(
    clippy::too_many_lines,
    reason = "End-to-end test covering full setup, publish, settle, and per-account assertions"
)]
#[tokio::test]
async fn test_balance_conditioned_republish() {
    init_tracing();
    let (_validators, node_url) = spawn_competing_validators(2).await;

    let sequencer_config = SequencerConfig {
        resubmit_interval: Duration::from_secs(3),
        ..SequencerConfig::default()
    };

    let signing_key_a = keygen();
    let admin_pk = signing_key_a.public_key();
    let channel_id = channel_id_from_key(&signing_key_a);
    let signing_key_b = keygen();
    let seq_b_pk = signing_key_b.public_key();
    let signing_key_c = keygen();
    let seq_c_pk = signing_key_c.public_key();

    authorize_keys(
        channel_id,
        signing_key_a.clone(),
        vec![admin_pk, seq_b_pk, seq_c_pk],
        60.into(),
        0.into(),
        node_url.clone(),
        sequencer_config.clone(),
    )
    .await;

    let initial_balances: std::collections::HashMap<String, i64> = [
        ("alice".to_owned(), 10i64),
        ("bob".to_owned(), 10i64),
        ("charlie".to_owned(), 10i64),
    ]
    .into_iter()
    .collect();

    let payloads_a = vec![
        make_balance_tx("a-alice", "alice", -6),
        make_balance_tx("a-bob", "bob", -3),
        make_balance_tx("a-charlie", "charlie", -2),
    ];
    let payloads_b = vec![
        make_balance_tx("b-alice", "alice", -5),
        make_balance_tx("b-bob", "bob", -4),
        make_balance_tx("b-charlie", "charlie", -8),
    ];
    let payloads_c = vec![
        make_balance_tx("c-alice", "alice", -4),
        make_balance_tx("c-bob", "bob", -7),
        make_balance_tx("c-charlie", "charlie", -1),
    ];

    let (seq_a, mut handle_a) = init_sequencer(
        channel_id,
        signing_key_a,
        node_url.clone(),
        sequencer_config.clone(),
    );
    let (seq_b, mut handle_b) = init_sequencer(
        channel_id,
        signing_key_b,
        node_url.clone(),
        sequencer_config.clone(),
    );
    let (seq_c, mut handle_c) = init_sequencer(
        channel_id,
        signing_key_c,
        node_url.clone(),
        sequencer_config,
    );

    let poll_a = spawn_balance_aware(seq_a, handle_a.clone(), initial_balances.clone());
    let poll_b = spawn_balance_aware(seq_b, handle_b.clone(), initial_balances.clone());
    let poll_c = spawn_balance_aware(seq_c, handle_c.clone(), initial_balances.clone());

    handle_a.wait_ready().await;
    handle_b.wait_ready().await;
    handle_c.wait_ready().await;

    publish_concurrently(vec![
        (handle_a, payloads_a.clone()),
        (handle_b, payloads_b.clone()),
        (handle_c, payloads_c.clone()),
    ])
    .await;

    // Wait for things to settle. With k=3 and 1s slots, finality is ~3s
    // per block. We wait long enough for retry cycles + finality.
    sleep(Duration::from_mins(1)).await;

    let indexer = ZoneIndexer::new(
        channel_id,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url),
    );
    let expected: HashSet<Vec<u8>> = payloads_a
        .iter()
        .chain(&payloads_b)
        .chain(&payloads_c)
        .cloned()
        .collect();
    let on_chain = scan_indexer_for_payloads(&indexer, &expected).await;

    // Compute per-account final balance from on-chain inscriptions.
    let mut final_balances: std::collections::HashMap<String, i64> = initial_balances.clone();
    for payload in &on_chain {
        if let Some((_, account, delta)) = parse_balance_tx(payload) {
            *final_balances.entry(account).or_insert(0) += delta;
        }
    }

    debug!(
        "On-chain {} inscriptions, final balances: {:?}",
        on_chain.len(),
        final_balances
    );

    for (account, balance) in &final_balances {
        assert!(
            *balance >= 0,
            "Account {account} balance went negative: {balance}. On-chain payloads: {:?}",
            on_chain
                .iter()
                .map(|p| String::from_utf8_lossy(p).to_string())
                .collect::<Vec<_>>()
        );
    }

    poll_a.abort();
    poll_b.abort();
    poll_c.abort();
}

/// Three sequencers concurrently publish the **same** payload bytes. They
/// race for the `parent_msg` slot; whoever loses gets an `orphaned` event and
/// republishes (with a fresh parent picked by the SDK). Eventually each
/// sequencer's inscription lands at a different position on chain.
///
/// Test invariant: exactly N (= number of sequencers) inscriptions with the
/// shared payload appear on chain — one per sequencer, no extras from
/// runaway republishes, none silently lost.
///
/// This exercises the simplified orphaned semantics (mine that's permanently
/// invalidated) on the dedup edge case where payload bytes alone can't
/// distinguish "mine vs theirs vs new republish" — only signer + `parent_msg`
/// + `msg_id` can.
#[tokio::test]
async fn test_concurrent_identical_payloads() {
    init_tracing();
    let (_validators, node_url) = spawn_competing_validators(2).await;

    let sequencer_config = SequencerConfig {
        resubmit_interval: Duration::from_secs(3),
        ..SequencerConfig::default()
    };

    let signing_key_a = keygen();
    let admin_pk = signing_key_a.public_key();
    let channel_id = channel_id_from_key(&signing_key_a);
    let signing_key_b = keygen();
    let seq_b_pk = signing_key_b.public_key();
    let signing_key_c = keygen();
    let seq_c_pk = signing_key_c.public_key();

    authorize_keys(
        channel_id,
        signing_key_a.clone(),
        vec![admin_pk, seq_b_pk, seq_c_pk],
        60.into(),
        0.into(),
        node_url.clone(),
        sequencer_config.clone(),
    )
    .await;

    // All three sequencers will publish this exact payload.
    let payload: Vec<u8> = b"shared-message".to_vec();

    let (seq_a, mut handle_a) = init_sequencer(
        channel_id,
        signing_key_a,
        node_url.clone(),
        sequencer_config.clone(),
    );
    let (seq_b, mut handle_b) = init_sequencer(
        channel_id,
        signing_key_b,
        node_url.clone(),
        sequencer_config.clone(),
    );
    let (seq_c, mut handle_c) = init_sequencer(
        channel_id,
        signing_key_c,
        node_url.clone(),
        sequencer_config,
    );

    let (finalized_tx, mut finalized_rx) = tokio::sync::mpsc::unbounded_channel();
    let poll_a = spawn_drive_republish(seq_a, handle_a.clone(), Some(finalized_tx.clone()));
    let poll_b = spawn_drive_republish(seq_b, handle_b.clone(), Some(finalized_tx.clone()));
    let poll_c = spawn_drive_republish(seq_c, handle_c.clone(), Some(finalized_tx));

    handle_a.wait_ready().await;
    handle_b.wait_ready().await;
    handle_c.wait_ready().await;

    let publishes_per_seq: usize = 10;
    let payloads: Vec<Vec<u8>> = vec![payload.clone(); publishes_per_seq];
    publish_concurrently(vec![
        (handle_a, payloads.clone()),
        (handle_b, payloads.clone()),
        (handle_c, payloads),
    ])
    .await;

    let indexer = ZoneIndexer::new(
        channel_id,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url),
    );

    let n_seqs: usize = 3;
    let expected_total: usize = n_seqs * publishes_per_seq;
    let expected: HashSet<Vec<u8>> = std::iter::once(payload.clone()).collect();

    // Deterministic wait: expected_total distinct tx_hashes with our payload
    // finalized.
    wait_for_finalized(
        &mut finalized_rx,
        &expected,
        expected_total,
        Duration::from_mins(10),
    )
    .await;

    // Re-scan indexer to verify the on-chain count matches.
    let on_chain = scan_indexer_for_payloads(&indexer, &expected).await;
    let final_count = on_chain.iter().filter(|p| **p == payload).count();

    assert_eq!(
        final_count, expected_total,
        "Expected exactly {expected_total} inscriptions with identical payload, got {final_count}"
    );

    poll_a.abort();
    poll_b.abort();
    poll_c.abort();
}

/// Test that resuming from a stale checkpoint works correctly.
///
/// Scenario: publish messages, save checkpoint, stop. Start fresh (no
/// checkpoint), publish more, stop. Resume from OLD checkpoint. The
/// stale pending txs should be reconciled — no duplicates on chain.
#[expect(
    clippy::too_many_lines,
    reason = "This test covers a full E2E flow with multiple steps, and breaking it up would not improve readability"
)]
#[tokio::test]
async fn test_sequencer_stale_checkpoint_resume() {
    init_tracing();
    let validators = spawn_validators(
        Some("test_sequencer_stale_checkpoint_resume"),
        1,
        |mut config| {
            config.deployment.time.slot_duration = Duration::from_secs(1);
            config
                .user
                .cryptarchia
                .service
                .bootstrap
                .prolonged_bootstrap_period = Duration::ZERO;
            config.deployment.cryptarchia.security_param = NonZero::new(3).unwrap();
            config.deployment.cryptarchia.slot_activation_coeff =
                NonNegativeRatio::new(1, 2.try_into().unwrap());
            config
        },
        3,
    )
    .await;
    let validator = &validators[0];
    let node_url = validator.url();

    let signing_key = keygen();
    let channel_id = channel_id_from_key(&signing_key);

    let sequencer_config = SequencerConfig {
        resubmit_interval: Duration::from_secs(3),
        ..SequencerConfig::default()
    };
    let indexer = ZoneIndexer::new(
        channel_id,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url.clone()),
    );

    // Phase 1: Publish and save checkpoint
    let (sequencer, mut handle) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key.clone(),
        NodeHttpClient::new(CommonHttpClient::new(None), node_url.clone()),
        sequencer_config.clone(),
        None,
    );
    let (drive_task, mut rx) = spawn_drive(sequencer);
    handle.wait_ready().await;

    let data_phase1: Vec<Vec<u8>> = vec![b"msg-1".to_vec(), b"msg-2".to_vec()];
    for data in &data_phase1 {
        handle
            .publish_message(data.clone())
            .await
            .expect("publish failed");
    }

    // Checkpoint arrives via Published event
    let mut stale_checkpoint = None;
    let mut published_count = 0;
    while let Some(event) = rx.recv().await {
        if let Event::Published { checkpoint, .. } = event {
            stale_checkpoint = Some(checkpoint);
            published_count += 1;
            if published_count >= data_phase1.len() {
                break;
            }
        }
    }
    let stale_checkpoint = stale_checkpoint.expect("should receive Published event");

    // Wait for phase 1 to finalize
    let mut received: Vec<Vec<u8>> = Vec::new();
    let mut last_zone_block = None;
    let start = std::time::Instant::now();
    loop {
        assert!(
            start.elapsed() <= Duration::from_mins(6),
            "Phase 1 finalization timeout"
        );
        let stream = indexer
            .next_messages(last_zone_block)
            .await
            .expect("indexer error");
        futures::pin_mut!(stream);

        while let Some((msg, slot)) = stream.next().await {
            if let ZoneMessage::Block(block) = msg {
                received.push(block.data.clone());
                last_zone_block = Some((block.id, slot));
            }
        }

        if received.len() >= data_phase1.len() {
            break;
        }
        sleep(Duration::from_millis(500)).await;
    }
    assert_eq!(
        received, data_phase1,
        "Phase 1 messages should match published order"
    );

    drive_task.abort();
    drop(handle);

    // Phase 2: Start FRESH, publish more
    let (sequencer, mut handle) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key.clone(),
        NodeHttpClient::new(CommonHttpClient::new(None), node_url.clone()),
        sequencer_config.clone(),
        None, // Fresh — no checkpoint
    );
    let (drive_task, _rx) = spawn_drive(sequencer);
    handle.wait_ready().await;
    let phase2_ready_lib_slot = validator
        .consensus_info(false)
        .await
        .cryptarchia_info
        .lib_slot;
    assert!(
        wait_for_lib_advance(validator, phase2_ready_lib_slot, Duration::from_mins(2)).await,
        "Phase 2 sequencer failed to observe a new LIB advancement after startup"
    );

    let data_phase2: Vec<Vec<u8>> = vec![b"msg-3".to_vec(), b"msg-4".to_vec()];
    for data in &data_phase2 {
        handle
            .publish_message(data.clone())
            .await
            .expect("publish failed");
    }

    // Wait for phase 2 to finalize
    let mut expected_all: Vec<Vec<u8>> = data_phase1
        .iter()
        .cloned()
        .chain(data_phase2.iter().cloned())
        .collect();
    let start = std::time::Instant::now();
    loop {
        assert!(
            start.elapsed() <= Duration::from_mins(6),
            "Phase 2 finalization timeout"
        );
        let stream = indexer
            .next_messages(last_zone_block)
            .await
            .expect("indexer error");
        futures::pin_mut!(stream);

        while let Some((msg, slot)) = stream.next().await {
            if let ZoneMessage::Block(block) = msg {
                received.push(block.data.clone());
                last_zone_block = Some((block.id, slot));
            }
        }

        if received.len() >= expected_all.len() {
            break;
        }
        sleep(Duration::from_millis(500)).await;
    }
    assert_eq!(
        received, expected_all,
        "Phase 1+2 messages should match published order"
    );

    drive_task.abort();
    drop(handle);

    // Phase 3: Resume from STALE checkpoint, publish more
    let (sequencer, mut handle) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url),
        sequencer_config,
        Some(stale_checkpoint), // Stale checkpoint from phase 1
    );
    let (drive_task, _rx) = spawn_drive(sequencer);
    handle.wait_ready().await;
    let phase3_ready_lib_slot = validator
        .consensus_info(false)
        .await
        .cryptarchia_info
        .lib_slot;
    assert!(
        wait_for_lib_advance(validator, phase3_ready_lib_slot, Duration::from_mins(2)).await,
        "Phase 3 sequencer failed to observe a new LIB advancement after startup"
    );

    let data_phase3: Vec<Vec<u8>> = vec![b"msg-5".to_vec()];
    for data in &data_phase3 {
        handle
            .publish_message(data.clone())
            .await
            .expect("publish failed");
    }

    // Verify all 5 messages appear, no duplicates
    expected_all.extend(data_phase3.iter().cloned());
    let start = std::time::Instant::now();
    loop {
        assert!(
            start.elapsed() <= Duration::from_mins(6),
            "Phase 3 finalization timeout"
        );
        let stream = indexer
            .next_messages(last_zone_block)
            .await
            .expect("indexer error");
        futures::pin_mut!(stream);

        while let Some((msg, slot)) = stream.next().await {
            if let ZoneMessage::Block(block) = msg {
                received.push(block.data.clone());
                last_zone_block = Some((block.id, slot));
            }
        }

        if received.len() >= expected_all.len() {
            break;
        }
        sleep(Duration::from_millis(500)).await;
    }
    assert_eq!(
        received, expected_all,
        "Phase 1+2+3 messages should match published order"
    );

    // Check no duplicates
    sleep(Duration::from_secs(30)).await;
    let mut all_payloads: Vec<Vec<u8>> = Vec::new();
    last_zone_block = None;
    loop {
        let stream = indexer
            .next_messages(last_zone_block)
            .await
            .expect("indexer error");
        futures::pin_mut!(stream);

        let mut msg_cnt = 0;
        while let Some((msg, slot)) = stream.next().await {
            msg_cnt += 1;
            if let ZoneMessage::Block(block) = msg {
                if expected_all.contains(&block.data) {
                    all_payloads.push(block.data.clone());
                }
                last_zone_block = Some((block.id, slot));
            }
        }

        if msg_cnt == 0 {
            break;
        }
    }

    let unique: HashSet<&Vec<u8>> = all_payloads.iter().collect();
    assert_eq!(
        unique.len(),
        all_payloads.len(),
        "No duplicate inscriptions"
    );
    assert_eq!(unique.len(), 5, "All 5 messages on chain");

    drive_task.abort();
}

#[tokio::test]
async fn test_subscribe_to_finalized_deposit() {
    // Setup network with faster block production
    let deposit_amount = 1;
    let validators = spawn_validators_with_extra_funding_notes(
        Some("test_subscribe_to_finalized_deposit"),
        2,
        [deposit_amount],
        |mut config| {
            config.deployment.time.slot_duration = Duration::from_secs(1);
            config
                .user
                .cryptarchia
                .service
                .bootstrap
                .prolonged_bootstrap_period = Duration::ZERO;
            config.deployment.cryptarchia.security_param = NonZero::new(3).unwrap();
            config.deployment.cryptarchia.slot_activation_coeff =
                NonNegativeRatio::new(1, 2.try_into().unwrap());
            config
        },
        3,
    )
    .await;
    let validator = &validators[0];
    let node_url = validator.url();

    let signing_key = keygen();
    let channel_id = channel_id_from_key(&signing_key);

    let (sequencer, mut handle) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url.clone()),
        SequencerConfig::default(),
        None,
    );
    let (drive_task, _rx) = spawn_drive(sequencer);
    handle.wait_ready().await;

    // Publish an inscription to create a channel
    let msg1 = b"initial inscription".to_vec();
    handle.publish_message(msg1.clone()).await.unwrap();

    let indexer = ZoneIndexer::new(
        channel_id,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url),
    );
    wait_for_zone_block(&indexer, msg1, Duration::from_mins(1)).await;

    // Now, submit a deposit directly to Bedrock
    let pk = validator.config().user.cryptarchia.leader.wallet.funding_pk;
    let (note_id, _) = get_note_with_value(validator, pk, deposit_amount)
        .await
        .expect("should find a note with sufficient balance for deposit");
    let deposit = DepositOp {
        channel_id,
        inputs: Inputs::new(vec![note_id]),
        metadata: format!("Mint {deposit_amount} to Alice in Zone").into_bytes(),
    };
    let pk = validator.config().user.cryptarchia.leader.wallet.funding_pk;
    submit_deposit(validator, deposit.clone(), pk).await;

    // Wait for the deposit to be finalized and detected by the ZoneIndexer
    wait_for_deposit(&indexer, &deposit, Duration::from_mins(2)).await;

    drive_task.abort();
}

#[tokio::test]
async fn test_atomic_deposit_inscription() {
    // Setup network with faster block production
    let validators = spawn_validators(
        Some("test_atomic_deposit_inscription"),
        2,
        |mut config| {
            config.deployment.time.slot_duration = Duration::from_secs(1);
            config
                .user
                .cryptarchia
                .service
                .bootstrap
                .prolonged_bootstrap_period = Duration::ZERO;
            config.deployment.cryptarchia.security_param = NonZero::new(3).unwrap();
            config.deployment.cryptarchia.slot_activation_coeff =
                NonNegativeRatio::new(1, 2.try_into().unwrap());
            config
        },
        3,
    )
    .await;
    let validator = &validators[0];
    let node_url = validator.url();

    let signing_key = keygen();
    let channel_id = channel_id_from_key(&signing_key);

    let (sequencer, mut handle) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url.clone()),
        SequencerConfig::default(),
        None,
    );
    let (drive_task, _rx) = spawn_drive(sequencer);
    handle.wait_ready().await;

    // Create a channel, so that a user can deposit into it.
    let msg1 = b"initial inscription".to_vec();
    handle.publish_message(msg1.clone()).await.unwrap();

    // Wait for the inscription to be accepted.
    // We wait for finalization even though it's not necessary,
    // because that's the only way we have currently.
    let indexer = ZoneIndexer::new(
        channel_id,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url),
    );
    wait_for_zone_block(&indexer, msg1, Duration::from_mins(1)).await;

    // Now, prepare a tx for deposit (from user) + inscription (from sequencer)
    let deposit_amount = 1u64;
    let pk = validator.config().user.cryptarchia.leader.wallet.funding_pk;
    let deposit_note = Note::new(deposit_amount, pk);
    let (note_id, note_value) = get_note(validator, pk, deposit_amount)
        .await
        .expect("should find a note with sufficient balance for deposit");

    let change = note_value.checked_sub(deposit_amount).unwrap();
    let transfer = TransferOp {
        inputs: Inputs::new(vec![note_id]),
        outputs: if change > 0 {
            Outputs::new(vec![deposit_note, Note::new(change, pk)])
        } else {
            Outputs::new(vec![deposit_note])
        },
    };
    let deposit = DepositOp {
        channel_id,
        inputs: Inputs::new(vec![
            transfer
                .outputs
                .utxo_by_index(0, &transfer)
                .expect("the first note of the transfer is the deposit_note")
                .id(),
        ]),
        metadata: format!("Mint {deposit_amount} to Alice in Zone").into_bytes(),
    };
    let inscription_data = format!("Mint {deposit_amount} to Alice").into_bytes();
    let (tx, msg_id, sequencer_sig) = handle
        .prepare_tx(
            vec![Op::Transfer(transfer), Op::ChannelDeposit(deposit.clone())],
            inscription_data.clone(),
        )
        .await
        .unwrap();

    // Ask the user to sign tx only for his own operations (deposit + transfer)
    let user_transfer_sig = sign_tx_zk(validator, &tx, vec![pk]).await;

    // Build a signed tx using signatures from user and sequencer
    let signed_tx = SignedMantleTx::new(
        tx,
        vec![
            OpProof::ZkSig(user_transfer_sig.clone()),
            OpProof::ZkSig(user_transfer_sig),
            OpProof::Ed25519Sig(sequencer_sig),
        ],
    )
    .unwrap();

    // Submit the signed tx via zone-sdk
    handle.submit_signed_tx(signed_tx, msg_id).await.unwrap();

    // Wait for deposit/inscription to be finalized and detected by the ZoneIndexer
    wait_for_deposit(&indexer, &deposit, Duration::from_mins(2)).await;
    wait_for_zone_block(&indexer, inscription_data, Duration::from_mins(2)).await;

    drive_task.abort();
}

#[tokio::test]
async fn test_subscribe_to_finalized_withdraw() {
    // Setup network with faster block production
    let deposit_amount = 3;
    let validators = spawn_validators_with_extra_funding_notes(
        Some("test_subscribe_to_finalized_withdraw"),
        2,
        [deposit_amount],
        |mut config| {
            config.deployment.time.slot_duration = Duration::from_secs(1);
            config
                .user
                .cryptarchia
                .service
                .bootstrap
                .prolonged_bootstrap_period = Duration::ZERO;
            config.deployment.cryptarchia.security_param = NonZero::new(3).unwrap();
            config.deployment.cryptarchia.slot_activation_coeff =
                NonNegativeRatio::new(1, 2.try_into().unwrap());
            config
        },
        3,
    )
    .await;
    let validator = &validators[0];
    let node_url = validator.url();

    let signing_key = keygen();
    let channel_id = channel_id_from_key(&signing_key);

    let (sequencer, mut handle) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url.clone()),
        SequencerConfig::default(),
        None,
    );
    let (drive_task, _rx) = spawn_drive(sequencer);
    handle.wait_ready().await;

    // Create a channel first
    let msg1 = b"initial inscription".to_vec();
    handle.publish_message(msg1.clone()).await.unwrap();

    // Wait for the inscription to be accepted.
    // We wait for finalization even though it's not necessary,
    // because that's the only way we have currently.
    let indexer = ZoneIndexer::new(
        channel_id,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url),
    );
    wait_for_zone_block(&indexer, msg1, Duration::from_mins(1)).await;

    // Deposit 3 into the channel
    let pk = validator.config().user.cryptarchia.leader.wallet.funding_pk;
    let (deposit_note_id, _) = get_note_with_value(validator, pk, deposit_amount)
        .await
        .expect("should find a note with sufficient balance for deposit");
    let deposit = DepositOp {
        channel_id,
        inputs: Inputs::new(vec![deposit_note_id]),
        metadata: b"Mint 3 to Alice in Zone".to_vec(),
    };
    submit_deposit(validator, deposit.clone(), pk).await;

    // Wait for the deposit to be finalized and detected by the ZoneIndexer
    wait_for_deposit(&indexer, &deposit, Duration::from_mins(2)).await;

    // Withdraw 1 from the channel
    let withdraw = ChannelWithdrawOp {
        channel_id,
        outputs: Outputs::new(vec![Note::new(2, pk)]),
        withdraw_nonce: 0,
    };
    let inscription_data = b"Burn 2".to_vec();
    let (tx, msg_id, inscription_proof) = handle
        .prepare_tx(
            vec![Op::ChannelWithdraw(withdraw.clone())],
            inscription_data.clone(),
        )
        .await
        .unwrap();

    // For this channel, a single sequencer signature is sufficient for withdraw,
    // because withdraw_threshold is 1.
    // We can actually reuse `inscription_proof`, but here we use
    // `SequencerHandle::sign_tx` to show how to sign tx built by other sequencers.
    let withdraw_proof = ChannelMultiSigProof::new(vec![IndexedSignature::new(
        0,
        handle.sign_tx(&tx).await.unwrap(),
    )])
    .unwrap();

    // Build a signed tx using signatures from user and sequencer
    let signed_tx = SignedMantleTx::new(
        tx,
        vec![
            OpProof::ChannelMultiSigProof(withdraw_proof),
            OpProof::Ed25519Sig(inscription_proof),
        ],
    )
    .unwrap();

    // Submit the signed tx via zone-sdk
    handle.submit_signed_tx(signed_tx, msg_id).await.unwrap();

    // Wait for withdraw/inscription to be finalized and detected by the ZoneIndexer
    wait_for_withdraw(&indexer, &withdraw, Duration::from_mins(2)).await;
    wait_for_zone_block(&indexer, inscription_data, Duration::from_mins(2)).await;

    drive_task.abort();
}

async fn spawn_validators(
    test_context: Option<&str>,
    count: usize,
    modify_run_config: impl Fn(RunConfig) -> RunConfig,
    target_block: u64,
) -> Vec<Validator> {
    spawn_validators_with_extra_funding_notes(
        test_context,
        count,
        [],
        modify_run_config,
        target_block,
    )
    .await
}

async fn spawn_validators_with_extra_funding_notes(
    test_context: Option<&str>,
    count: usize,
    funding_note_values: impl IntoIterator<Item = Value>,
    modify_run_config: impl Fn(RunConfig) -> RunConfig,
    target_block: u64,
) -> Vec<Validator> {
    let (configs, genesis_block) = create_general_configs(count, test_context);
    let genesis_block = add_extra_funding_notes_to_genesis(
        &configs,
        genesis_block,
        funding_note_values,
        test_context,
    );
    let deployment_settings = e2e_deployment_settings_with_genesis_block(&genesis_block);
    let configs: Vec<_> = configs
        .into_iter()
        .map(|c| {
            let config = create_validator_config(c, deployment_settings.clone());
            modify_run_config(config)
        })
        .collect();

    let validators = join_all(configs.into_iter().map(Validator::spawn))
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("Failed to spawn validators");

    let timeout_duration = max_block_propagation_time(
        target_block as u32,
        validators.len() as u64,
        &validators[0].config().deployment,
        3.0,
    );
    wait_for_validators_mode_and_height(
        &validators,
        ChainServiceMode::Started(State::Online),
        target_block,
        timeout_duration,
    )
    .await;

    validators
}

fn add_extra_funding_notes_to_genesis(
    configs: &[GeneralConfig],
    genesis_tx: GenesisBlock,
    values: impl IntoIterator<Item = Value>,
    test_context: Option<&str>,
) -> GenesisBlock {
    let values = values.into_iter().collect::<Vec<_>>();
    if values.is_empty() {
        return genesis_tx;
    }

    let funding_pk = configs[0].consensus_config.funding_pk;
    let mut transfer_op = genesis_tx.genesis_tx().genesis_transfer().clone();
    transfer_op
        .outputs
        .as_mut()
        .extend(values.into_iter().map(|value| Note::new(value, funding_pk)));

    let providers = configs
        .iter()
        .map(|config| {
            let (blend_config, provider_sk, zk_sk) = &config.blend_config;

            ProviderInfo {
                service_type: ServiceType::BlendNetwork,
                provider_sk: provider_sk.clone(),
                zk_sk: zk_sk.clone(),
                locator: Locator::new_unchecked(
                    blend_config.core.backend.listening_address.clone(),
                ),
                note: config.consensus_config.blend_note.clone(),
            }
        })
        .collect();

    create_genesis_block_with_declarations(transfer_op, providers, test_context)
}

async fn wait_for_zone_block(
    indexer: &ZoneIndexer<NodeHttpClient>,
    expected_data: Vec<u8>,
    timeout_duration: Duration,
) {
    timeout(timeout_duration, async {
        let mut last_zone_block = None;
        loop {
            let stream = indexer.next_messages(last_zone_block).await.unwrap();
            futures::pin_mut!(stream);

            let stream = indexer
                .next_messages(last_zone_block)
                .await
                .expect("indexer error");
            futures::pin_mut!(stream);

            while let Some((msg, slot)) = stream.next().await {
                if let ZoneMessage::Block(block) = msg {
                    if block.data == expected_data {
                        println!("Found expected inscription: {expected_data:?}");
                        return;
                    }
                    last_zone_block = Some((block.id, slot));
                }
            }

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("timed out");
}

async fn wait_for_deposit(
    indexer: &ZoneIndexer<NodeHttpClient>,
    expected: &DepositOp,
    timeout_duration: Duration,
) {
    timeout(timeout_duration, async {
        let mut last_zone_block = None;
        loop {
            let stream = indexer.next_messages(last_zone_block).await.unwrap();
            futures::pin_mut!(stream);

            while let Some((msg, slot)) = stream.next().await {
                match msg {
                    ZoneMessage::Block(block) => {
                        last_zone_block = Some((block.id, slot));
                    }
                    ZoneMessage::Deposit(deposit) => {
                        if deposit.inputs == expected.inputs
                            && deposit.metadata == expected.metadata
                        {
                            println!(
                                "Found expected deposit in indexer: amount={:?} metadata={:?}",
                                deposit.inputs, deposit.metadata
                            );
                            return;
                        }
                    }
                    ZoneMessage::Withdraw(_) => {}
                }
            }

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("timed out");
}

async fn wait_for_withdraw(
    indexer: &ZoneIndexer<NodeHttpClient>,
    expected: &ChannelWithdrawOp,
    timeout_duration: Duration,
) {
    timeout(timeout_duration, async {
        let mut last_zone_block = None;
        loop {
            let stream = indexer.next_messages(last_zone_block).await.unwrap();
            futures::pin_mut!(stream);

            while let Some((msg, slot)) = stream.next().await {
                match msg {
                    ZoneMessage::Block(block) => {
                        last_zone_block = Some((block.id, slot));
                    }
                    ZoneMessage::Withdraw(withdraw) => {
                        if withdraw.outputs == expected.outputs {
                            println!(
                                "Found expected withdraw in indexer: amount={:?}",
                                withdraw.outputs,
                            );
                            return;
                        }
                    }
                    ZoneMessage::Deposit(_) => {}
                }
            }

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("timed out");
}

async fn get_note(
    validator: &Validator,
    pk: ZkPublicKey,
    min_value: Value,
) -> Option<(NoteId, Value)> {
    get_wallet_balance(validator, pk)
        .await
        .notes
        .into_iter()
        .find(|(_, value)| *value >= min_value)
}

async fn get_note_with_value(
    validator: &Validator,
    pk: ZkPublicKey,
    expected_value: Value,
) -> Option<(NoteId, Value)> {
    get_wallet_balance(validator, pk)
        .await
        .notes
        .into_iter()
        .find(|(_, value)| *value == expected_value)
}

async fn get_wallet_balance(validator: &Validator, pk: ZkPublicKey) -> WalletBalanceResponseBody {
    let resp = reqwest::Client::new()
        .get(format!(
            "http://{}/wallet/{}/balance",
            validator.config().user.api.backend.listen_address,
            hex::encode(lb_groth16::fr_to_bytes(&pk.into()))
        ))
        .send()
        .await
        .expect("balance request should not fail");

    assert!(
        resp.status().is_success(),
        "balance request should succeed: status={}",
        resp.status()
    );

    resp.json()
        .await
        .expect("balance response should be valid JSON")
}

async fn sign_tx_zk(validator: &Validator, tx: &MantleTx, pks: Vec<ZkPublicKey>) -> ZkSignature {
    let resp = reqwest::Client::new()
        .post(format!(
            "http://{}/wallet/sign/zk",
            validator.config().user.api.backend.listen_address,
        ))
        .json(&WalletSignTxZkRequestBody {
            tx_hash: tx.hash(),
            pks,
        })
        .send()
        .await
        .expect("sign API should not fail");

    assert!(
        resp.status().is_success(),
        "sign API should succeed: status={}, resp={}",
        resp.status(),
        resp.text().await.unwrap_or_default(),
    );

    let body: WalletSignTxZkResponseBody = resp
        .json()
        .await
        .expect("sign response should be valid JSON");

    body.sig
}

async fn submit_deposit(validator: &Validator, deposit: DepositOp, pk: ZkPublicKey) {
    let body = ChannelDepositRequestBody {
        tip: None,
        deposit,
        change_public_key: pk,
        funding_public_keys: vec![pk],
        max_tx_fee: 10.into(),
    };
    let resp = reqwest::Client::new()
        .post(format!(
            "http://{}/channel/deposit",
            validator.config().user.api.backend.listen_address
        ))
        .json(&body)
        .send()
        .await
        .expect("request should not fail");
    assert!(
        resp.status().is_success(),
        "request should succeed, got status: {} body: {}",
        resp.status(),
        resp.text().await.unwrap_or_default(),
    );
}
