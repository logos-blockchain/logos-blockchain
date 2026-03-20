use std::{collections::HashSet, num::NonZero, time::Duration};

use futures::future::join_all;
use lb_core::mantle::ops::channel::ChannelId;
use lb_key_management_system_service::keys::Ed25519Key;
use lb_zone_sdk::{
    indexer::ZoneIndexer,
    sequencer::{InscriptionStatus, SequencerConfig, ZoneSequencer},
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
    let sequencer = ZoneSequencer::init_with_config(
        channel_id,
        signing_key,
        node_url.clone(),
        None,
        sequencer_config,
        None, // Fresh start, no checkpoint
    );

    // Publish inscriptions (with retry until sequencer is initialized)
    let test_data: Vec<Vec<u8>> = vec![
        b"Hello, Zone!".to_vec(),
        b"Second message".to_vec(),
        b"Third message".to_vec(),
    ];

    let publish_start = std::time::Instant::now();
    let publish_timeout = Duration::from_secs(30);

    for data in &test_data {
        loop {
            assert!(
                publish_start.elapsed() <= publish_timeout,
                "Timeout waiting for sequencer to initialize"
            );

            match sequencer.publish(data.clone()).await {
                Ok(_) => break,
                Err(_) => {
                    // Sequencer not ready yet, wait and retry
                    sleep(Duration::from_millis(500)).await;
                }
            }
        }
    }

    // Poll indexer until all expected payloads are seen.
    // Messages need to be included in a block and then finalized (k=5
    // confirmations). With 1s slot time, this should be relatively fast.
    let indexer = ZoneIndexer::new(channel_id, node_url, None);

    let expected: HashSet<Vec<u8>> = test_data.iter().cloned().collect();
    let mut seen: HashSet<Vec<u8>> = HashSet::new();
    let mut seen_ordered: Vec<Vec<u8>> = Vec::new();
    let mut cursor = None;

    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(180);

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

    let set_keys_tx_hash = sequencer
        .set_keys(vec![admin_pk, second_pk])
        .await
        .expect("set_keys should succeed");

    // Poll status until finalized (same finality path as inscriptions).
    let status_start = std::time::Instant::now();
    let status_timeout = Duration::from_secs(180);

    loop {
        assert!(
            status_start.elapsed() <= status_timeout,
            "Timeout waiting for set_keys transaction to finalize"
        );

        let status = sequencer
            .status(set_keys_tx_hash)
            .await
            .expect("status should succeed");

        if matches!(status, InscriptionStatus::Finalized) {
            break;
        }

        sleep(Duration::from_millis(500)).await;
    }
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
    let sequencer = ZoneSequencer::init_with_config(
        channel_id,
        signing_key.clone(),
        node_url.clone(),
        None,
        sequencer_config.clone(),
        None, // Fresh start
    );

    let test_data_phase1: Vec<Vec<u8>> = vec![b"Message 1".to_vec(), b"Message 2".to_vec()];

    let publish_timeout = Duration::from_secs(30);
    let publish_start = std::time::Instant::now();
    let mut last_checkpoint = None;

    for data in &test_data_phase1 {
        loop {
            assert!(
                publish_start.elapsed() <= publish_timeout,
                "Timeout waiting for sequencer to initialize"
            );

            match sequencer.publish(data.clone()).await {
                Ok(result) => {
                    // Save checkpoint from publish result
                    last_checkpoint = Some(result.checkpoint);
                    break;
                }
                Err(_) => {
                    sleep(Duration::from_millis(500)).await;
                }
            }
        }
    }

    // Get checkpoint before "stopping" the sequencer
    let checkpoint = last_checkpoint.expect("Should have checkpoint after publishing");

    // Drop the old sequencer (simulating stop)
    drop(sequencer);

    // Phase 2: Resume with checkpoint and publish more messages
    let sequencer = ZoneSequencer::init_with_config(
        channel_id,
        signing_key,
        node_url.clone(),
        None,
        sequencer_config,
        Some(checkpoint), // Resume from checkpoint
    );

    let test_data_phase2: Vec<Vec<u8>> = vec![b"Message 3".to_vec(), b"Message 4".to_vec()];

    let publish_start = std::time::Instant::now();
    for data in &test_data_phase2 {
        loop {
            assert!(
                publish_start.elapsed() <= publish_timeout,
                "Timeout waiting for sequencer to initialize"
            );

            match sequencer.publish(data.clone()).await {
                Ok(_) => break,
                Err(_) => {
                    sleep(Duration::from_millis(500)).await;
                }
            }
        }
    }

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
    let timeout = Duration::from_secs(180);

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
}
