use std::{collections::HashSet, num::NonZero, time::Duration};

use futures::future::join_all;
use lb_core::mantle::ops::channel::ChannelId;
use lb_key_management_system_service::keys::Ed25519Key;
use lb_zone_sdk::{
    indexer::ZoneIndexer,
    sequencer::{Event, SequencerConfig, ZoneSequencer},
};
use logos_blockchain_tests::{
    nodes::{Validator, create_validator_config},
    topology::configs::{
        create_general_configs, deployment::e2e_deployment_settings_with_genesis_tx,
    },
};
use rand::{Rng as _, thread_rng};
use serial_test::serial;
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
            if info.height >= target_height {
                return;
            }
            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .is_ok()
}

#[tokio::test]
#[serial]
async fn test_sequencer_publish_and_indexer_read() {
    init_tracing();
    // Use custom config with faster block production for test reliability:
    // - slot_duration: 1s (faster slots)
    // - security_param (k): 5 (fewer blocks needed for LIB to advance)
    let (configs, genesis_tx) = create_general_configs(2);
    let deployment_settings = e2e_deployment_settings_with_genesis_tx(genesis_tx);
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
            config.deployment.cryptarchia.security_param = NonZero::new(5).unwrap();
            config
        })
        .collect();

    let validators: Vec<Validator> = join_all(configs.into_iter().map(Validator::spawn))
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("Failed to spawn validators");

    let validator = &validators[0];

    // Wait for the chain to produce at least one block.
    // Use generous timeout since leader election is probabilistic.
    assert!(
        wait_for_height(validator, 1, Duration::from_secs(120)).await,
        "Chain should produce the first block"
    );
    let node_url = validator.url();

    // Random signing key per test run to avoid channel collisions
    let mut key_bytes = [0u8; 32];
    thread_rng().fill(&mut key_bytes);
    let signing_key = Ed25519Key::from_bytes(&key_bytes);
    let admin_pk = signing_key.public_key();
    let channel_id = channel_id_from_key(&signing_key);

    // Use short resubmit interval matching fast block production (1s slots).
    // Default 30s is too slow - if a tx gets orphaned, we miss many opportunities.
    let sequencer_config = SequencerConfig {
        resubmit_interval: Duration::from_secs(3),
        ..SequencerConfig::default()
    };
    let (mut sequencer, mut handle) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key,
        node_url.clone(),
        None,
        sequencer_config,
        None, // Fresh start, no checkpoint
    );

    // Spawn a task to drive the sequencer — handle reorgs by re-publishing
    let reorg_handle = handle.clone();
    let poll_task = tokio::spawn(async move {
        loop {
            if let Some(Event::ChannelUpdate {
                invalidated,
                adopted,
                ..
            }) = sequencer.next_event().await
            {
                let adopted_payloads: HashSet<Vec<u8>> =
                    adopted.into_iter().map(|a| a.payload).collect();
                for inv in invalidated {
                    if !adopted_payloads.contains(&inv.payload) {
                        drop(reorg_handle.publish(inv.payload).await);
                    }
                }
            }
        }
    });

    // Wait for sequencer to be ready, then publish inscriptions.
    // Each payload is tagged with a random ID for reorg deduplication.
    let test_data: Vec<Vec<u8>> = vec![
        tag_payload("Hello, Zone!"),
        tag_payload("Second message"),
        tag_payload("Third message"),
    ];

    publish_all(&mut handle, &test_data).await;

    // Poll indexer until all expected payloads are seen.
    // Messages need to be included in a block and then finalized (k=5
    // confirmations). With 1s slot time, this should be relatively fast.
    let indexer = ZoneIndexer::new(channel_id, node_url, None);

    let expected: HashSet<Vec<u8>> = test_data.iter().cloned().collect();
    let mut seen: HashSet<Vec<u8>> = HashSet::new();
    let mut seen_ordered: Vec<Vec<u8>> = Vec::new();
    let mut cursor = None;

    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(360);

    loop {
        assert!(
            start.elapsed() <= timeout,
            "Timeout waiting for indexer to return all messages"
        );

        let result = indexer
            .next_messages(cursor, 100)
            .await
            .expect("next_messages should succeed");

        for msg in &result.messages {
            if expected.contains(&msg.data) && !seen.contains(&msg.data) {
                seen.insert(msg.data.clone());
                seen_ordered.push(msg.data.clone());
            }
        }

        cursor = Some(result.cursor);

        if seen == expected {
            break;
        }

        sleep(Duration::from_millis(500)).await;
    }

    // Verify ordering: messages should appear in the order they were published
    assert_eq!(seen_ordered.len(), test_data.len());

    for (i, expected_data) in test_data.iter().enumerate() {
        assert_eq!(&seen_ordered[i], expected_data);
    }

    // --- Test set_keys: update channel's accredited keys ---
    // Generate a second key and add it alongside the original admin key.
    let mut key_bytes2 = [0u8; 32];
    thread_rng().fill(&mut key_bytes2);
    let second_key = Ed25519Key::from_bytes(&key_bytes2);
    let second_pk = second_key.public_key();

    let finalized = handle
        .set_keys(vec![admin_pk, second_pk])
        .await
        .expect("set_keys should succeed");

    // Wait for set_keys transaction to finalize
    tokio::time::timeout(Duration::from_secs(360), finalized)
        .await
        .expect("Timeout waiting for set_keys to finalize")
        .expect("set_keys finalization failed");

    // Clean up
    poll_task.abort();
}

#[tokio::test]
#[serial]
async fn test_sequencer_checkpoint_resume() {
    init_tracing();
    // Setup network with faster block production
    let (configs, genesis_tx) = create_general_configs(2);
    let deployment_settings = e2e_deployment_settings_with_genesis_tx(genesis_tx);
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
            config.deployment.cryptarchia.security_param = NonZero::new(5).unwrap();
            config
        })
        .collect();

    let validators: Vec<Validator> = join_all(configs.into_iter().map(Validator::spawn))
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("Failed to spawn validators");

    let validator = &validators[0];

    assert!(
        wait_for_height(validator, 1, Duration::from_secs(120)).await,
        "Chain should produce the first block"
    );
    let node_url = validator.url();

    // Random signing key per test run
    let mut key_bytes = [0u8; 32];
    thread_rng().fill(&mut key_bytes);
    let signing_key = Ed25519Key::from_bytes(&key_bytes);
    let channel_id = channel_id_from_key(&signing_key);

    let sequencer_config = SequencerConfig {
        resubmit_interval: Duration::from_secs(3),
        ..SequencerConfig::default()
    };

    // Phase 1: Start fresh sequencer and publish messages
    let (mut sequencer, mut handle) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key.clone(),
        node_url.clone(),
        None,
        sequencer_config.clone(),
        None, // Fresh start
    );

    // Spawn polling task with reorg handling
    let reorg_handle = handle.clone();
    let poll_task = tokio::spawn(async move {
        loop {
            if let Some(Event::ChannelUpdate {
                invalidated,
                adopted,
                ..
            }) = sequencer.next_event().await
            {
                let adopted_payloads: HashSet<Vec<u8>> =
                    adopted.into_iter().map(|a| a.payload).collect();
                for inv in invalidated {
                    if !adopted_payloads.contains(&inv.payload) {
                        drop(reorg_handle.publish(inv.payload).await);
                    }
                }
            }
        }
    });

    let test_data_phase1: Vec<Vec<u8>> = vec![tag_payload("Message 1"), tag_payload("Message 2")];

    handle.wait_ready().await;
    let mut last_result = None;
    for data in &test_data_phase1 {
        last_result = Some(
            handle
                .publish(data.clone())
                .await
                .expect("publish should succeed"),
        );
    }

    // Get checkpoint from last publish result
    let checkpoint = last_result
        .expect("Should have result after publishing")
        .checkpoint;

    // Stop the sequencer (simulating stop)
    poll_task.abort();
    drop(handle);

    // Phase 2: Resume with checkpoint and publish more messages
    let (mut sequencer, mut handle) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key,
        node_url.clone(),
        None,
        sequencer_config,
        Some(checkpoint), // Resume from checkpoint
    );

    let reorg_handle = handle.clone();
    let poll_task = tokio::spawn(async move {
        loop {
            if let Some(Event::ChannelUpdate {
                invalidated,
                adopted,
                ..
            }) = sequencer.next_event().await
            {
                let adopted_payloads: HashSet<Vec<u8>> =
                    adopted.into_iter().map(|a| a.payload).collect();
                for inv in invalidated {
                    if !adopted_payloads.contains(&inv.payload) {
                        drop(reorg_handle.publish(inv.payload).await);
                    }
                }
            }
        }
    });

    let test_data_phase2: Vec<Vec<u8>> = vec![tag_payload("Message 3"), tag_payload("Message 4")];

    publish_all(&mut handle, &test_data_phase2).await;

    // Verify all messages (from both phases) are indexed
    let indexer = ZoneIndexer::new(channel_id, node_url, None);

    let all_test_data: Vec<Vec<u8>> = test_data_phase1
        .into_iter()
        .chain(test_data_phase2)
        .collect();
    let expected: HashSet<Vec<u8>> = all_test_data.iter().cloned().collect();
    let mut seen: HashSet<Vec<u8>> = HashSet::new();
    let mut cursor = None;

    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(360);

    loop {
        assert!(
            start.elapsed() <= timeout,
            "Timeout waiting for indexer to return all messages"
        );

        let result = indexer
            .next_messages(cursor, 100)
            .await
            .expect("next_messages should succeed");

        for msg in &result.messages {
            if expected.contains(&msg.data) {
                seen.insert(msg.data.clone());
            }
        }

        cursor = Some(result.cursor);

        if seen == expected {
            break;
        }

        sleep(Duration::from_millis(500)).await;
    }

    assert_eq!(
        seen.len(),
        all_test_data.len(),
        "All messages from both phases should be indexed"
    );

    // Clean up
    poll_task.abort();
}

/// Subscribe to a sequencer's events and re-publish orphaned inscriptions.
///
/// Uses `handle.subscribe()` — the intended usage pattern for client apps.
/// The event loop must be driven separately (e.g. via `spawn_sequencer_poll`
/// or `ZoneSequencer::spawn`).
fn spawn_republish_handler(
    handle: &lb_zone_sdk::sequencer::SequencerHandle,
) -> tokio::task::JoinHandle<()> {
    let mut events = handle.subscribe();
    let handle = handle.clone();
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(Event::ChannelUpdate {
                    invalidated,
                    adopted,
                    ..
                }) => {
                    let adopted_payloads: HashSet<Vec<u8>> =
                        adopted.into_iter().map(|a| a.payload).collect();
                    for inv in invalidated {
                        if !adopted_payloads.contains(&inv.payload) {
                            debug!(
                                "Re-publishing orphaned: {:?}",
                                String::from_utf8_lossy(&inv.payload)
                            );
                            let h = handle.clone(); // TODO: remove spawn when publish is fire-and-forget
                            tokio::spawn(async move {
                                if let Err(e) = h.publish(inv.payload).await {
                                    debug!("Failed to re-publish: {e}");
                                }
                            });
                        }
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    })
}

/// Drive the sequencer event loop in the background.
fn spawn_sequencer_poll(mut sequencer: ZoneSequencer) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            sequencer.next_event().await;
        }
    })
}

/// Helper: wait for readiness then publish all payloads.
async fn publish_all(handle: &mut lb_zone_sdk::sequencer::SequencerHandle, payloads: &[Vec<u8>]) {
    handle.wait_ready().await;
    for data in payloads {
        handle
            .publish(data.clone())
            .await
            .expect("publish should succeed after wait_ready");
    }
}

/// Helper: wait for all expected payloads to appear in the indexer.
async fn wait_for_indexer(
    indexer: &ZoneIndexer,
    expected: &HashSet<Vec<u8>>,
    timeout_duration: Duration,
) {
    let mut seen: HashSet<Vec<u8>> = HashSet::new();
    let mut cursor = None;
    let start = std::time::Instant::now();

    loop {
        assert!(
            start.elapsed() <= timeout_duration,
            "Timeout waiting for indexer to return all messages"
        );

        let result = indexer
            .next_messages(cursor, 100)
            .await
            .expect("next_messages should succeed");

        for msg in &result.messages {
            let payload_str = String::from_utf8_lossy(&msg.data);
            if expected.contains(&msg.data) {
                let is_new = seen.insert(msg.data.clone());
                if is_new {
                    debug!("Found payload: {payload_str}");
                } else {
                    debug!("DUPLICATE payload: {payload_str}");
                }
            }
        }

        cursor = Some(result.cursor);

        if seen == *expected {
            break;
        }

        if !seen.is_empty() {
            // Log which payloads are still missing
            let missing: Vec<String> = expected
                .iter()
                .filter(|p| !seen.contains(*p))
                .map(|p| String::from_utf8_lossy(p).to_string())
                .collect();
            debug!(
                "{}/{} found, missing: {:?}",
                seen.len(),
                expected.len(),
                missing
            );
        }

        sleep(Duration::from_millis(500)).await;
    }
}

/// Helper: tag a message with a random ID for reorg deduplication.
fn tag_payload(msg: &str) -> Vec<u8> {
    format!("{:016x}:{msg}", rand::random::<u64>()).into_bytes()
}

#[tokio::test]
#[serial]
async fn test_sequential_multi_sequencer() {
    init_tracing();
    // Setup: two validators, fast blocks
    let (configs, genesis_tx) = create_general_configs(2);
    let deployment_settings = e2e_deployment_settings_with_genesis_tx(genesis_tx);
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
            config.deployment.cryptarchia.security_param = NonZero::new(5).unwrap();
            config
        })
        .collect();

    let validators: Vec<Validator> = join_all(configs.into_iter().map(Validator::spawn))
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("Failed to spawn validators");

    let validator = &validators[0];
    assert!(
        wait_for_height(validator, 1, Duration::from_secs(120)).await,
        "Chain should produce the first block"
    );
    let node_url = validator.url();

    let sequencer_config = SequencerConfig {
        resubmit_interval: Duration::from_secs(3),
        ..SequencerConfig::default()
    };

    // Create two signing keys — SeqA is the channel creator/admin
    let mut key_bytes_a = [0u8; 32];
    thread_rng().fill(&mut key_bytes_a);
    let signing_key_a = Ed25519Key::from_bytes(&key_bytes_a);
    let admin_pk = signing_key_a.public_key();
    let channel_id = channel_id_from_key(&signing_key_a);

    let mut key_bytes_b = [0u8; 32];
    thread_rng().fill(&mut key_bytes_b);
    let signing_key_b = Ed25519Key::from_bytes(&key_bytes_b);
    let seq_b_pk = signing_key_b.public_key();

    // --- Phase 1: SeqA publishes a1, a2, a3 ---
    let (sequencer_a, mut handle_a) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key_a.clone(),
        node_url.clone(),
        None,
        sequencer_config.clone(),
        None,
    );
    let poll_a = spawn_sequencer_poll(sequencer_a);

    let phase1_data: Vec<Vec<u8>> = vec![tag_payload("a1"), tag_payload("a2"), tag_payload("a3")];
    publish_all(&mut handle_a, &phase1_data).await;

    let indexer = ZoneIndexer::new(channel_id, node_url.clone(), None);
    let expected_phase1: HashSet<Vec<u8>> = phase1_data.iter().cloned().collect();
    wait_for_indexer(&indexer, &expected_phase1, Duration::from_secs(360)).await;

    // --- SeqA adds SeqB's key via set_keys ---
    let finalized = handle_a
        .set_keys(vec![admin_pk, seq_b_pk])
        .await
        .expect("set_keys should succeed");
    timeout(Duration::from_secs(360), finalized)
        .await
        .expect("Timeout waiting for set_keys to finalize")
        .expect("set_keys finalization failed");

    // Stop SeqA
    poll_a.abort();
    drop(handle_a);

    // --- Phase 2: SeqB publishes b1, b2, b3 ---
    let (sequencer_b, mut handle_b) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key_b,
        node_url.clone(),
        None,
        sequencer_config.clone(),
        None, // Fresh start — SeqB discovers channel tip from chain
    );
    let poll_b = spawn_sequencer_poll(sequencer_b);

    let phase2_data: Vec<Vec<u8>> = vec![tag_payload("b1"), tag_payload("b2"), tag_payload("b3")];
    publish_all(&mut handle_b, &phase2_data).await;

    let mut expected_phase2 = expected_phase1.clone();
    expected_phase2.extend(phase2_data.iter().cloned());
    wait_for_indexer(&indexer, &expected_phase2, Duration::from_secs(360)).await;

    // Stop SeqB
    poll_b.abort();
    drop(handle_b);

    // --- Phase 3: SeqA resumes and publishes a4, a5, a6 ---
    // SeqA starts fresh (no checkpoint) — must discover current channel tip
    let (sequencer_a, mut handle_a) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key_a,
        node_url.clone(),
        None,
        sequencer_config,
        None, // Fresh start — discovers current channel tip
    );
    let poll_a = spawn_sequencer_poll(sequencer_a);

    let phase3_data: Vec<Vec<u8>> = vec![tag_payload("a4"), tag_payload("a5"), tag_payload("a6")];
    publish_all(&mut handle_a, &phase3_data).await;

    let mut expected_all = expected_phase2;
    expected_all.extend(phase3_data.iter().cloned());
    wait_for_indexer(&indexer, &expected_all, Duration::from_secs(360)).await;

    // Verify all 9 inscriptions are on chain in the expected order:
    // a1, a2, a3 (SeqA phase1), b1, b2, b3 (SeqB phase2), a4, a5, a6 (SeqA phase3)
    let expected_order: Vec<Vec<u8>> = phase1_data
        .iter()
        .chain(phase2_data.iter())
        .chain(phase3_data.iter())
        .cloned()
        .collect();
    let mut cursor = None;
    let mut on_chain_order = Vec::new();
    loop {
        let result = indexer
            .next_messages(cursor, 100)
            .await
            .expect("next_messages should succeed");
        for msg in &result.messages {
            if expected_all.contains(&msg.data) {
                on_chain_order.push(msg.data.clone());
            }
        }
        cursor = Some(result.cursor);
        if result.messages.is_empty() {
            break;
        }
    }
    assert_eq!(
        on_chain_order, expected_order,
        "Inscriptions should appear in expected sequential order"
    );

    // Clean up
    poll_a.abort();
}

#[tokio::test]
#[serial]
async fn test_concurrent_multi_sequencer() {
    init_tracing();
    // Use case B — ad-hoc: three sequencers publish concurrently on the same
    // channel with set_keys authorization. SeqA creates the channel, then
    // all three publish in parallel. Each sequencer's inscriptions maintain
    // their internal order but may be interleaved with each other on chain.

    // Setup: two validators, all sequencers share one node to avoid
    // immutable block index divergence between validators.
    let (configs, genesis_tx) = create_general_configs(2);
    let deployment_settings = e2e_deployment_settings_with_genesis_tx(genesis_tx);
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
            config.deployment.cryptarchia.security_param = NonZero::new(5).unwrap();
            config
        })
        .collect();

    let validators: Vec<Validator> = join_all(configs.into_iter().map(Validator::spawn))
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("Failed to spawn validators");

    assert!(
        wait_for_height(&validators[0], 1, Duration::from_secs(120)).await,
        "Chain should produce the first block"
    );
    let node_url = validators[0].url();

    let sequencer_config = SequencerConfig {
        resubmit_interval: Duration::from_secs(30),
        ..SequencerConfig::default()
    };

    // Create three signing keys — SeqA is the channel creator/admin
    let mut key_bytes_a = [0u8; 32];
    thread_rng().fill(&mut key_bytes_a);
    let signing_key_a = Ed25519Key::from_bytes(&key_bytes_a);
    let admin_pk = signing_key_a.public_key();
    let channel_id = channel_id_from_key(&signing_key_a);

    let mut key_bytes_b = [0u8; 32];
    thread_rng().fill(&mut key_bytes_b);
    let signing_key_b = Ed25519Key::from_bytes(&key_bytes_b);
    let seq_b_pk = signing_key_b.public_key();

    let mut key_bytes_c = [0u8; 32];
    thread_rng().fill(&mut key_bytes_c);
    let signing_key_c = Ed25519Key::from_bytes(&key_bytes_c);
    let seq_c_pk = signing_key_c.public_key();

    // --- Phase 1: SeqA creates channel and authorizes all three via set_keys ---
    debug!("Phase 1: Starting SeqA for set_keys");
    let (sequencer_a, mut handle_a) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key_a,
        node_url.clone(),
        None,
        sequencer_config.clone(),
        None,
    );
    let poll_a = spawn_sequencer_poll(sequencer_a);

    handle_a.wait_ready().await;
    debug!("Phase 1: SeqA ready, submitting set_keys");
    let finalized = handle_a
        .set_keys(vec![admin_pk, seq_b_pk, seq_c_pk])
        .await
        .expect("set_keys should succeed");
    timeout(Duration::from_secs(360), finalized)
        .await
        .expect("Timeout waiting for set_keys to finalize")
        .expect("set_keys finalization failed");

    // Stop SeqA — will restart concurrently with B and C
    debug!("Phase 1: set_keys finalized, stopping SeqA");
    poll_a.abort();
    drop(handle_a);

    // Prepare payloads before starting sequencers
    let data_a: Vec<Vec<u8>> = vec![tag_payload("a1"), tag_payload("a2"), tag_payload("a3")];
    let data_b: Vec<Vec<u8>> = vec![tag_payload("b1"), tag_payload("b2"), tag_payload("b3")];
    let data_c: Vec<Vec<u8>> = vec![tag_payload("c1"), tag_payload("c2"), tag_payload("c3")];

    // --- Phase 2: Start all three sequencers with intent tracking ---
    debug!("Phase 2: Starting 3 sequencers concurrently");
    let (sequencer_a, mut handle_a) = ZoneSequencer::init_with_config(
        channel_id,
        Ed25519Key::from_bytes(&key_bytes_a),
        node_url.clone(),
        None,
        sequencer_config.clone(),
        None,
    );

    let (sequencer_b, mut handle_b) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key_b,
        node_url.clone(),
        None,
        sequencer_config.clone(),
        None,
    );

    let (sequencer_c, mut handle_c) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key_c,
        node_url.clone(),
        None,
        sequencer_config,
        None,
    );

    // Start poll tasks — next_event() must be running to process publish requests.
    let poll_a = {
        spawn_sequencer_poll(sequencer_a);
        spawn_republish_handler(&handle_a)
    };
    let poll_b = {
        spawn_sequencer_poll(sequencer_b);
        spawn_republish_handler(&handle_b)
    };
    let poll_c = {
        spawn_sequencer_poll(sequencer_c);
        spawn_republish_handler(&handle_c)
    };

    // Wait for all three to be ready
    handle_a.wait_ready().await;
    handle_b.wait_ready().await;
    handle_c.wait_ready().await;
    debug!("Phase 2: All 3 sequencers ready");

    // Phase 3: Publish initial inscriptions concurrently.
    debug!("Phase 3: Publishing 9 inscriptions concurrently");
    tokio::join!(
        async {
            for d in &data_a {
                handle_a.publish(d.clone()).await.expect("publish failed");
            }
        },
        async {
            for d in &data_b {
                handle_b.publish(d.clone()).await.expect("publish failed");
            }
        },
        async {
            for d in &data_c {
                handle_c.publish(d.clone()).await.expect("publish failed");
            }
        },
    );

    // Phase 4: Wait for all 9 inscriptions to appear on chain
    debug!("Phase 4: Waiting for all 9 inscriptions in indexer");
    let indexer = ZoneIndexer::new(channel_id, node_url, None);
    let mut expected_all: HashSet<Vec<u8>> = HashSet::new();
    expected_all.extend(data_a.iter().cloned());
    expected_all.extend(data_b.iter().cloned());
    expected_all.extend(data_c.iter().cloned());
    assert_eq!(expected_all.len(), 9);

    wait_for_indexer(&indexer, &expected_all, Duration::from_secs(1200)).await;

    // Wait for enough blocks so any late re-published duplicates would have
    // landed. With k=5 and 1s slots, finality is ~5 blocks. We wait 30s
    // (~30 blocks) to be safe — enough for resubmit cycles (3s) and any
    // in-flight transactions to settle.
    sleep(Duration::from_secs(30)).await;

    let mut all_payloads: Vec<Vec<u8>> = Vec::new();
    let mut cursor = None;
    loop {
        let result = indexer
            .next_messages(cursor, 100)
            .await
            .expect("next_messages should succeed");
        for msg in &result.messages {
            if expected_all.contains(&msg.data) {
                all_payloads.push(msg.data.clone());
            }
        }
        if result.messages.is_empty() {
            break;
        }
        cursor = Some(result.cursor);
    }

    let unique: HashSet<&Vec<u8>> = all_payloads.iter().collect();
    assert_eq!(
        unique.len(),
        all_payloads.len(),
        "Duplicate inscriptions detected on chain: expected {} unique, got {} total",
        unique.len(),
        all_payloads.len(),
    );
    assert_eq!(unique.len(), 9, "Expected exactly 9 inscriptions on chain");

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
    sequencer: ZoneSequencer,
    handle: lb_zone_sdk::sequencer::SequencerHandle,
    discarded: DiscardedSet,
) -> tokio::task::JoinHandle<()> {
    let mut events = handle.subscribe();
    spawn_sequencer_poll(sequencer);

    tokio::spawn(async move {
        let mut max_seen_on_chain: Option<Vec<u8>> = None;

        loop {
            match events.recv().await {
                Ok(Event::ChannelUpdate {
                    invalidated,
                    adopted,
                    ..
                }) => {
                    for a in &adopted {
                        if max_seen_on_chain
                            .as_ref()
                            .is_none_or(|max| a.payload > *max)
                        {
                            max_seen_on_chain = Some(a.payload.clone());
                        }
                    }

                    for inv in invalidated {
                        // Re-publish if our payload is larger than the max
                        // on chain — it correctly goes at the end in sorted
                        // order. Drop if smaller — it lost its position and
                        // can't be inserted earlier (channel is append-only).
                        let should_republish = max_seen_on_chain
                            .as_ref()
                            .is_some_and(|max| inv.payload >= *max);

                        if should_republish {
                            debug!(
                                "Sorted policy: re-publishing {:?} (larger than max {:?})",
                                String::from_utf8_lossy(&inv.payload),
                                max_seen_on_chain
                                    .as_ref()
                                    .map(|m| String::from_utf8_lossy(m).to_string()),
                            );
                            let h = handle.clone(); // TODO: remove spawn when publish is fire-and-forget
                            tokio::spawn(async move {
                                if let Err(e) = h.publish(inv.payload).await {
                                    debug!("Failed to re-publish: {e}");
                                }
                            });
                        } else {
                            debug!(
                                "Sorted policy: dropping {:?} (< max {:?})",
                                String::from_utf8_lossy(&inv.payload),
                                max_seen_on_chain
                                    .as_ref()
                                    .map(|m| String::from_utf8_lossy(m).to_string()),
                            );
                            discarded.lock().await.insert(inv.payload);
                        }
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    })
}

#[tokio::test]
#[serial]
async fn test_sorted_conflict_resolution() {
    init_tracing();
    // Two sequencers publish interleaved sorted payloads concurrently.
    // Custom policy: "smallest wins" — when a conflict occurs, the
    // lexicographically smaller payload keeps its position; the larger
    // one is dropped. The on-chain result must be sorted.

    let (configs, genesis_tx) = create_general_configs(2);
    let deployment_settings = e2e_deployment_settings_with_genesis_tx(genesis_tx);
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
            config.deployment.cryptarchia.security_param = NonZero::new(5).unwrap();
            config
        })
        .collect();

    let validators: Vec<Validator> = join_all(configs.into_iter().map(Validator::spawn))
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("Failed to spawn validators");

    assert!(
        wait_for_height(&validators[0], 1, Duration::from_secs(120)).await,
        "Chain should produce the first block"
    );
    let node_url = validators[0].url();

    let sequencer_config = SequencerConfig {
        resubmit_interval: Duration::from_secs(3),
        ..SequencerConfig::default()
    };

    // SeqA is the channel admin
    let mut key_bytes_a = [0u8; 32];
    thread_rng().fill(&mut key_bytes_a);
    let signing_key_a = Ed25519Key::from_bytes(&key_bytes_a);
    let admin_pk = signing_key_a.public_key();
    let channel_id = channel_id_from_key(&signing_key_a);

    let mut key_bytes_b = [0u8; 32];
    thread_rng().fill(&mut key_bytes_b);
    let signing_key_b = Ed25519Key::from_bytes(&key_bytes_b);
    let seq_b_pk = signing_key_b.public_key();

    // Phase 1: SeqA creates channel and authorizes SeqB
    debug!("Phase 1: set_keys");
    let (sequencer_a, mut handle_a) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key_a,
        node_url.clone(),
        None,
        sequencer_config.clone(),
        None,
    );
    let poll_a = spawn_sequencer_poll(sequencer_a);

    handle_a.wait_ready().await;
    let finalized = handle_a
        .set_keys(vec![admin_pk, seq_b_pk])
        .await
        .expect("set_keys should succeed");
    timeout(Duration::from_secs(360), finalized)
        .await
        .expect("Timeout waiting for set_keys to finalize")
        .expect("set_keys finalization failed");

    poll_a.abort();
    drop(handle_a);

    // Phase 2: Both sequencers publish interleaved sorted payloads.
    // All payloads are unique across both sets — no UUIDs needed.
    // SeqA: "aa", "cc", "ee", "gg", "ii"
    // SeqB: "bb", "dd", "ff", "hh", "jj"
    debug!("Phase 2: Starting both sequencers with sorted policy");
    let data_a: Vec<Vec<u8>> = ["aa", "cc", "ee", "gg", "ii"]
        .iter()
        .map(|s| s.as_bytes().to_vec())
        .collect();
    let data_b: Vec<Vec<u8>> = ["bb", "dd", "ff", "hh", "jj"]
        .iter()
        .map(|s| s.as_bytes().to_vec())
        .collect();

    let (sequencer_a, mut handle_a) = ZoneSequencer::init_with_config(
        channel_id,
        Ed25519Key::from_bytes(&key_bytes_a),
        node_url.clone(),
        None,
        sequencer_config.clone(),
        None,
    );
    let (sequencer_b, mut handle_b) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key_b,
        node_url.clone(),
        None,
        sequencer_config,
        None,
    );

    let discarded: DiscardedSet = std::sync::Arc::new(tokio::sync::Mutex::new(HashSet::new()));
    let poll_a = spawn_sequencer_sorted_policy(
        sequencer_a,
        handle_a.clone(),
        DiscardedSet::clone(&discarded),
    );
    let poll_b = spawn_sequencer_sorted_policy(
        sequencer_b,
        handle_b.clone(),
        DiscardedSet::clone(&discarded),
    );

    handle_a.wait_ready().await;
    handle_b.wait_ready().await;
    debug!("Phase 2: Both sequencers ready, publishing");

    // Publish concurrently
    tokio::join!(
        async {
            for d in &data_a {
                handle_a.publish(d.clone()).await.expect("publish failed");
            }
        },
        async {
            for d in &data_b {
                handle_b.publish(d.clone()).await.expect("publish failed");
            }
        },
    );

    // Phase 3: Poll indexer until we see all non-discarded payloads.
    debug!("Phase 3: Polling indexer for finalized inscriptions");
    let indexer = ZoneIndexer::new(channel_id, node_url, None);
    let all_payloads: HashSet<Vec<u8>> = data_a.iter().chain(data_b.iter()).cloned().collect();
    let mut on_chain: Vec<Vec<u8>> = Vec::new();
    let mut cursor = None;
    let start = std::time::Instant::now();

    loop {
        assert!(
            start.elapsed() <= Duration::from_secs(600),
            "Timeout waiting for inscriptions to finalize"
        );

        let expected_count = 10 - discarded.lock().await.len();
        if on_chain.len() >= expected_count && expected_count > 0 {
            break;
        }

        let result = indexer
            .next_messages(cursor, 100)
            .await
            .expect("next_messages should succeed");
        for msg in &result.messages {
            if all_payloads.contains(&msg.data) && !on_chain.contains(&msg.data) {
                on_chain.push(msg.data.clone());
                debug!(
                    "Indexer found: {:?} ({}/{})",
                    String::from_utf8_lossy(&msg.data),
                    on_chain.len(),
                    expected_count,
                );
            }
        }
        cursor = Some(result.cursor);
        sleep(Duration::from_millis(500)).await;
    }

    debug!(
        "On-chain payloads: {:?}",
        on_chain
            .iter()
            .map(|p| String::from_utf8_lossy(p).to_string())
            .collect::<Vec<_>>()
    );

    // No duplicates
    let unique: HashSet<&Vec<u8>> = on_chain.iter().collect();
    assert_eq!(
        unique.len(),
        on_chain.len(),
        "Duplicate inscriptions detected on chain"
    );

    // The key invariant: whatever survived on chain must be sorted.
    let is_sorted = on_chain.windows(2).all(|w| w[0] <= w[1]);
    assert!(
        is_sorted,
        "On-chain payloads must be sorted, got: {:?}",
        on_chain
            .iter()
            .map(|p| String::from_utf8_lossy(p).to_string())
            .collect::<Vec<_>>()
    );

    // At least some payloads should have survived
    assert!(
        !on_chain.is_empty(),
        "At least some payloads should be on chain"
    );

    // Accounting: on-chain + discarded == total published
    let discarded_set = discarded.lock().await;
    debug!(
        "{} on chain + {} discarded = {} (of 10 published)",
        on_chain.len(),
        discarded_set.len(),
        on_chain.len() + discarded_set.len(),
    );

    // No overlap between on-chain and discarded
    let on_chain_set: HashSet<Vec<u8>> = on_chain.iter().cloned().collect();
    let overlap: Vec<_> = on_chain_set.intersection(&discarded_set).collect();
    assert!(
        overlap.is_empty(),
        "Payload both on chain and discarded: {:?}",
        overlap
            .iter()
            .map(|p| String::from_utf8_lossy(p))
            .collect::<Vec<_>>()
    );

    assert_eq!(
        on_chain.len() + discarded_set.len(),
        10,
        "on_chain + discarded must equal total published"
    );
    drop(discarded_set);

    // Clean up
    poll_a.abort();
    poll_b.abort();
}
