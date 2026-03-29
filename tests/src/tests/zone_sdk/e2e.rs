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

/// Helper: spawn a sequencer poll task with reorg handling.
/// Intent-based client state for suffix reconstruction.
/// Tracks which payloads have been adopted on chain and rebuilds
/// the remaining suffix when invalidated.
struct IntentState {
    intents: Vec<Vec<u8>>,
    committed: usize,
}

impl IntentState {
    fn new(intents: Vec<Vec<u8>>) -> Self {
        Self {
            intents,
            committed: 0,
        }
    }

    /// Advance committed prefix based on adopted payloads.
    fn mark_adopted(&mut self, adopted: &HashSet<Vec<u8>>) {
        while self.committed < self.intents.len()
            && adopted.contains(&self.intents[self.committed])
        {
            self.committed += 1;
        }
    }

    /// Get the suffix that still needs to be published (from committed onward).
    fn pending_suffix(&self) -> &[Vec<u8>] {
        &self.intents[self.committed..]
    }

    fn is_complete(&self) -> bool {
        self.committed >= self.intents.len()
    }
}

/// Spawn poll + suffix rebuild tasks for a sequencer with intent tracking.
fn spawn_sequencer_with_intents(
    mut sequencer: ZoneSequencer,
    handle: lb_zone_sdk::sequencer::SequencerHandle,
    intents: Vec<Vec<u8>>,
) -> tokio::task::JoinHandle<()> {
    // Channel to signal the rebuilder to submit the current suffix.
    let (rebuild_tx, mut rebuild_rx) = tokio::sync::mpsc::channel::<Vec<Vec<u8>>>(1);

    // Rebuilder: publishes a suffix in order, one at a time.
    let rebuild_handle = handle.clone();
    tokio::spawn(async move {
        while let Some(suffix) = rebuild_rx.recv().await {
            for payload in suffix {
                if rebuild_handle.publish(payload).await.is_err() {
                    break; // sequencer dropped, stop
                }
            }
        }
    });

    // Poll task: drives next_event(), tracks intent state, triggers rebuilds.
    tokio::spawn(async move {
        let mut state = IntentState::new(intents);

        loop {
            if let Some(Event::ChannelUpdate {
                adopted,
                invalidated,
                ..
            }) = sequencer.next_event().await
            {
                let adopted_payloads: HashSet<Vec<u8>> =
                    adopted.into_iter().map(|a| a.payload).collect();

                // Advance committed prefix
                state.mark_adopted(&adopted_payloads);

                if state.is_complete() {
                    continue;
                }

                // If any of our intents were invalidated, rebuild suffix
                let our_invalidated = invalidated
                    .iter()
                    .any(|inv| state.pending_suffix().contains(&inv.payload));

                if our_invalidated {
                    let suffix = state.pending_suffix().to_vec();
                    eprintln!(
                        "[CLIENT] Rebuilding suffix: committed={}/{}, suffix_len={}",
                        state.committed,
                        state.intents.len(),
                        suffix.len()
                    );
                    // Send suffix to rebuilder — if channel is full, previous
                    // rebuild is still running and will be superseded by the
                    // next ChannelUpdate.
                    let _ = rebuild_tx.try_send(suffix);
                }
            }
        }
    })
}

/// Simple poll task for sequencers without intent tracking (e.g. phase 1).
fn spawn_sequencer_poll(
    mut sequencer: ZoneSequencer,
    _handle: lb_zone_sdk::sequencer::SequencerHandle,
) -> tokio::task::JoinHandle<()> {
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
            if expected.contains(&msg.data) {
                seen.insert(msg.data.clone());
            }
        }

        cursor = Some(result.cursor);

        if seen == *expected {
            break;
        }

        if !result.messages.is_empty() || !seen.is_empty() {
            eprintln!("[INDEXER] Found {}/{} payloads, msgs_in_batch={}", seen.len(), expected.len(), result.messages.len());
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
    let poll_a = spawn_sequencer_poll(sequencer_a, handle_a.clone());

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
    let poll_b = spawn_sequencer_poll(sequencer_b, handle_b.clone());

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
    let poll_a = spawn_sequencer_poll(sequencer_a, handle_a.clone());

    let phase3_data: Vec<Vec<u8>> = vec![tag_payload("a4"), tag_payload("a5"), tag_payload("a6")];
    publish_all(&mut handle_a, &phase3_data).await;

    let mut expected_all = expected_phase2;
    expected_all.extend(phase3_data.iter().cloned());
    wait_for_indexer(&indexer, &expected_all, Duration::from_secs(360)).await;

    // Verify all 9 inscriptions are on chain
    assert_eq!(expected_all.len(), 9);

    // Clean up
    poll_a.abort();
}

#[tokio::test]
#[serial]
async fn test_concurrent_multi_sequencer() {
    // Use case B — ad-hoc: three sequencers publish concurrently on the same
    // channel with set_keys authorization. SeqA creates the channel, then
    // all three publish in parallel. Each sequencer's inscriptions maintain
    // their internal order but may be interleaved with each other on chain.

    // Setup: three validators (one per sequencer), fast blocks
    let (configs, genesis_tx) = create_general_configs(3);
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
    let node_url_a = validators[0].url();
    let node_url_b = validators[1].url();
    let node_url_c = validators[2].url();

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
    eprintln!("[TEST] Phase 1: Starting SeqA for set_keys");
    let (sequencer_a, mut handle_a) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key_a,
        node_url_a.clone(),
        None,
        sequencer_config.clone(),
        None,
    );
    let poll_a = spawn_sequencer_poll(sequencer_a, handle_a.clone());

    handle_a.wait_ready().await;
    eprintln!("[TEST] Phase 1: SeqA ready, submitting set_keys");
    let finalized = handle_a
        .set_keys(vec![admin_pk, seq_b_pk, seq_c_pk])
        .await
        .expect("set_keys should succeed");
    timeout(Duration::from_secs(360), finalized)
        .await
        .expect("Timeout waiting for set_keys to finalize")
        .expect("set_keys finalization failed");

    // Stop SeqA — will restart concurrently with B and C
    eprintln!("[TEST] Phase 1: set_keys finalized, stopping SeqA");
    poll_a.abort();
    drop(handle_a);

    // Prepare payloads before starting sequencers
    let data_a: Vec<Vec<u8>> = vec![tag_payload("a1"), tag_payload("a2"), tag_payload("a3")];
    let data_b: Vec<Vec<u8>> = vec![tag_payload("b1"), tag_payload("b2"), tag_payload("b3")];
    let data_c: Vec<Vec<u8>> = vec![tag_payload("c1"), tag_payload("c2"), tag_payload("c3")];

    // --- Phase 2: Start all three sequencers with intent tracking ---
    eprintln!("[TEST] Phase 2: Starting 3 sequencers concurrently");
    let (sequencer_a, mut handle_a) = ZoneSequencer::init_with_config(
        channel_id,
        Ed25519Key::from_bytes(&key_bytes_a),
        node_url_a.clone(),
        None,
        sequencer_config.clone(),
        None,
    );

    let (sequencer_b, mut handle_b) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key_b,
        node_url_b.clone(),
        None,
        sequencer_config.clone(),
        None,
    );

    let (sequencer_c, mut handle_c) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key_c,
        node_url_c.clone(),
        None,
        sequencer_config,
        None,
    );

    // Start intent-tracking poll tasks BEFORE publishing — next_event()
    // must be running to process publish requests.
    let poll_a = spawn_sequencer_with_intents(sequencer_a, handle_a.clone(), data_a.clone());
    let poll_b = spawn_sequencer_with_intents(sequencer_b, handle_b.clone(), data_b.clone());
    let poll_c = spawn_sequencer_with_intents(sequencer_c, handle_c.clone(), data_c.clone());

    // Wait for all three to be ready
    handle_a.wait_ready().await;
    handle_b.wait_ready().await;
    handle_c.wait_ready().await;
    eprintln!("[TEST] Phase 2: All 3 sequencers ready");

    // Phase 3: Publish initial inscriptions concurrently.
    // The intent-tracking poll tasks handle suffix reconstruction
    // when competing inscriptions invalidate our chain.
    eprintln!("[TEST] Phase 3: Publishing 9 inscriptions concurrently");
    tokio::join!(
        async {
            for d in &data_a {
                handle_a
                    .publish(d.clone())
                    .await
                    .expect("SeqA publish failed");
            }
        },
        async {
            for d in &data_b {
                handle_b
                    .publish(d.clone())
                    .await
                    .expect("SeqB publish failed");
            }
        },
        async {
            for d in &data_c {
                handle_c
                    .publish(d.clone())
                    .await
                    .expect("SeqC publish failed");
            }
        },
    );

    // Phase 4: Wait for all 9 inscriptions to appear on chain
    eprintln!("[TEST] Phase 4: Waiting for all 9 inscriptions in indexer");
    let indexer = ZoneIndexer::new(channel_id, node_url_a, None);
    let mut expected_all: HashSet<Vec<u8>> = HashSet::new();
    expected_all.extend(data_a.iter().cloned());
    expected_all.extend(data_b.iter().cloned());
    expected_all.extend(data_c.iter().cloned());
    assert_eq!(expected_all.len(), 9);

    wait_for_indexer(&indexer, &expected_all, Duration::from_secs(600)).await;

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
