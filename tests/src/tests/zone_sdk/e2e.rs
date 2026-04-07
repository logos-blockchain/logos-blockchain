use std::{collections::HashSet, num::NonZero, time::Duration};

use futures::{StreamExt as _, future::join_all};
use lb_common_http_client::CommonHttpClient;
use lb_core::mantle::ops::channel::ChannelId;
use lb_key_management_system_service::keys::Ed25519Key;
use lb_zone_sdk::{
    ZoneMessage,
    adapter::NodeHttpClient,
    indexer::ZoneIndexer,
    sequencer::{SequencerConfig, ZoneSequencer},
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
    let (sequencer, mut handle) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url.clone()),
        sequencer_config,
        None, // Fresh start, no checkpoint
    );

    let poll_task = sequencer.spawn();
    handle.wait_ready().await;

    let test_data: Vec<Vec<u8>> = vec![
        b"Hello, Zone!".to_vec(),
        b"Second message".to_vec(),
        b"Third message".to_vec(),
    ];

    for data in &test_data {
        handle.publish(data.clone()).await.expect("publish failed");
    }

    // Poll indexer until all expected payloads are seen.
    // Messages need to be included in a block and then finalized (k=5
    // confirmations). With 1s slot time, this should be relatively fast.
    let indexer = ZoneIndexer::new(
        channel_id,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url),
    );

    let expected: HashSet<Vec<u8>> = test_data.iter().cloned().collect();
    let mut seen: HashSet<Vec<u8>> = HashSet::new();
    let mut seen_ordered: Vec<Vec<u8>> = Vec::new();
    let mut last_zone_block = None;

    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(360);

    loop {
        assert!(
            start.elapsed() <= timeout,
            "Timeout waiting for indexer to return all messages"
        );

        let stream = indexer
            .next_messages(last_zone_block)
            .await
            .expect("next_messages should succeed");
        futures::pin_mut!(stream);

        while let Some((msg, slot)) = stream.next().await {
            if let ZoneMessage::Block(block) = msg {
                if expected.contains(&block.data) && !seen.contains(&block.data) {
                    seen.insert(block.data.clone());
                    seen_ordered.push(block.data.clone());
                }
                last_zone_block = Some((block.id, slot));
            }
        }

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

    let (_result, finalized) = handle
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
    let (sequencer, mut handle) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key.clone(),
        NodeHttpClient::new(CommonHttpClient::new(None), node_url.clone()),
        sequencer_config.clone(),
        None, // Fresh start
    );

    let poll_task = sequencer.spawn();
    handle.wait_ready().await;

    let test_data_phase1: Vec<Vec<u8>> = vec![b"Message 1".to_vec(), b"Message 2".to_vec()];

    let mut last_publish_result = None;
    for data in &test_data_phase1 {
        let result = handle.publish(data.clone()).await.expect("publish failed");
        last_publish_result = Some(result);
    }

    let checkpoint = last_publish_result.unwrap().checkpoint;

    // Stop the sequencer (simulating stop)
    poll_task.abort();
    drop(handle);

    // Phase 2: Resume with checkpoint and publish more messages
    let (sequencer, mut handle) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url.clone()),
        sequencer_config,
        Some(checkpoint), // Resume from checkpoint
    );

    let poll_task = sequencer.spawn();
    handle.wait_ready().await;

    let test_data_phase2: Vec<Vec<u8>> = vec![b"Message 3".to_vec(), b"Message 4".to_vec()];
    for data in &test_data_phase2 {
        handle.publish(data.clone()).await.expect("publish failed");
    }

    // Verify all messages (from both phases) are indexed
    let indexer = ZoneIndexer::new(
        channel_id,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url),
    );

    let all_test_data: Vec<Vec<u8>> = test_data_phase1
        .into_iter()
        .chain(test_data_phase2)
        .collect();
    let expected: HashSet<Vec<u8>> = all_test_data.iter().cloned().collect();
    let mut seen: HashSet<Vec<u8>> = HashSet::new();
    let mut last_zone_block = None;

    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(360);

    loop {
        assert!(
            start.elapsed() <= timeout,
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
                    seen.insert(block.data.clone());
                }
                last_zone_block = Some((block.id, slot));
            }
        }

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

/// Test that resuming from a stale checkpoint works correctly.
///
/// Scenario: publish messages, save checkpoint, stop. Start fresh (no
/// checkpoint), publish more, stop. Resume from OLD checkpoint. The
/// stale pending txs should be reconciled — no duplicates on chain.
#[tokio::test]
#[serial]
async fn test_sequencer_stale_checkpoint_resume() {
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

    let mut key_bytes = [0u8; 32];
    thread_rng().fill(&mut key_bytes);
    let signing_key = Ed25519Key::from_bytes(&key_bytes);
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
    let poll_task = sequencer.spawn();
    handle.wait_ready().await;

    let data_phase1: Vec<Vec<u8>> = vec![b"msg-1".to_vec(), b"msg-2".to_vec()];
    let mut last_result = None;
    for data in &data_phase1 {
        let r = handle.publish(data.clone()).await.expect("publish failed");
        last_result = Some(r);
    }
    let stale_checkpoint = last_result.unwrap().checkpoint;

    // Wait for phase 1 to finalize
    let expected: HashSet<Vec<u8>> = data_phase1.iter().cloned().collect();
    let mut seen: HashSet<Vec<u8>> = HashSet::new();
    let mut last_zone_block = None;
    let start = std::time::Instant::now();
    loop {
        assert!(
            start.elapsed() <= Duration::from_secs(360),
            "Phase 1 finalization timeout"
        );
        let stream = indexer
            .next_messages(last_zone_block)
            .await
            .expect("indexer error");
        futures::pin_mut!(stream);

        while let Some((msg, slot)) = stream.next().await {
            if let ZoneMessage::Block(block) = msg {
                if expected.contains(&block.data) {
                    seen.insert(block.data.clone());
                }
                last_zone_block = Some((block.id, slot));
            }
        }

        if seen == expected {
            break;
        }
        sleep(Duration::from_millis(500)).await;
    }

    poll_task.abort();
    drop(handle);

    // Phase 2: Start FRESH, publish more
    let (sequencer, mut handle) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key.clone(),
        NodeHttpClient::new(CommonHttpClient::new(None), node_url.clone()),
        sequencer_config.clone(),
        None, // Fresh — no checkpoint
    );
    let poll_task = sequencer.spawn();
    handle.wait_ready().await;

    let data_phase2: Vec<Vec<u8>> = vec![b"msg-3".to_vec(), b"msg-4".to_vec()];
    for data in &data_phase2 {
        handle.publish(data.clone()).await.expect("publish failed");
    }

    // Wait for phase 2 to finalize
    let mut expected_all: HashSet<Vec<u8>> = expected;
    expected_all.extend(data_phase2.iter().cloned());
    let start = std::time::Instant::now();
    loop {
        assert!(
            start.elapsed() <= Duration::from_secs(360),
            "Phase 2 finalization timeout"
        );
        let stream = indexer
            .next_messages(last_zone_block)
            .await
            .expect("indexer error");
        futures::pin_mut!(stream);

        while let Some((msg, slot)) = stream.next().await {
            if let ZoneMessage::Block(block) = msg {
                if expected_all.contains(&block.data) {
                    seen.insert(block.data.clone());
                }
                last_zone_block = Some((block.id, slot));
            }
        }

        if seen == expected_all {
            break;
        }
        sleep(Duration::from_millis(500)).await;
    }

    poll_task.abort();
    drop(handle);

    // Phase 3: Resume from STALE checkpoint, publish more
    let (sequencer, mut handle) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url),
        sequencer_config,
        Some(stale_checkpoint), // Stale checkpoint from phase 1
    );
    let poll_task = sequencer.spawn();
    handle.wait_ready().await;

    let data_phase3: Vec<Vec<u8>> = vec![b"msg-5".to_vec()];
    for data in &data_phase3 {
        handle.publish(data.clone()).await.expect("publish failed");
    }

    // Verify all 5 messages appear, no duplicates
    expected_all.extend(data_phase3.iter().cloned());
    let start = std::time::Instant::now();
    loop {
        assert!(
            start.elapsed() <= Duration::from_secs(360),
            "Phase 3 finalization timeout"
        );
        let stream = indexer
            .next_messages(last_zone_block)
            .await
            .expect("indexer error");
        futures::pin_mut!(stream);

        while let Some((msg, slot)) = stream.next().await {
            if let ZoneMessage::Block(block) = msg {
                if expected_all.contains(&block.data) {
                    seen.insert(block.data.clone());
                }
                last_zone_block = Some((block.id, slot));
            }
        }

        if seen == expected_all {
            break;
        }
        sleep(Duration::from_millis(500)).await;
    }

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

    poll_task.abort();
}
