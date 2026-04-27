use std::{collections::HashSet, num::NonZero, time::Duration};

use futures::{StreamExt as _, future::join_all};
use lb_common_http_client::{CommonHttpClient, Slot};
use lb_core::{
    mantle::{
        GenesisTx as _, MantleTx, Note, NoteId, Op, OpProof, Value,
        genesis_tx::GenesisTx as GenesisTransaction,
        ledger::{Inputs, Outputs},
        ops::{
            channel::{ChannelId, deposit::DepositOp, withdraw::ChannelWithdrawOp},
            transfer::TransferOp,
        },
    },
    proofs::channel_withdraw_proof::{ChannelWithdrawProof, WithdrawSignature},
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
    sequencer::{SequencerConfig, ZoneSequencer},
};
use logos_blockchain_tests::{
    nodes::{Validator, create_validator_config},
    topology::configs::{
        GeneralConfig,
        consensus::{ProviderInfo, create_genesis_tx_with_declarations},
        create_general_configs,
        deployment::e2e_deployment_settings_with_genesis_tx,
    },
};
use rand::{Rng as _, thread_rng};
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

async fn wait_for_lib_advance(
    validator: &Validator,
    initial_lib_slot: Slot,
    duration: Duration,
) -> bool {
    timeout(duration, async {
        loop {
            let info = validator.consensus_info(false).await;
            if info.lib_slot > initial_lib_slot {
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
    // Use custom config with faster block production for test reliability:
    // - slot_duration: 1s (faster slots)
    // - security_param (k): 5 (fewer blocks needed for LIB to advance)
    let validators = spawn_validators(
        Some("test_sequencer_publish_and_indexer_read"),
        2,
        |mut config| {
            config.deployment.time.slot_duration = Duration::from_secs(1);
            config
                .user
                .cryptarchia
                .service
                .bootstrap
                .prolonged_bootstrap_period = Duration::ZERO;
            config.deployment.cryptarchia.security_param = NonZero::new(5).unwrap();
            config
        },
        1,
    )
    .await;
    let validator = &validators[0];
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
        handle
            .publish_message(data.clone())
            .await
            .expect("publish failed");
    }

    // Poll indexer until all expected payloads are seen.
    // Messages need to be included in a block and then finalized (k=5
    // confirmations). With 1s slot time, this should be relatively fast.
    let indexer = ZoneIndexer::new(
        channel_id,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url),
    );

    let mut received: Vec<Vec<u8>> = Vec::new();
    let mut last_zone_block = None;

    let start = std::time::Instant::now();
    let timeout = Duration::from_mins(6);

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
                received.push(block.data.clone());
                last_zone_block = Some((block.id, slot));
            }
        }

        if received.len() >= test_data.len() {
            break;
        }

        sleep(Duration::from_millis(500)).await;
    }

    assert_eq!(received, test_data, "Messages should match published order");

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
    tokio::time::timeout(Duration::from_mins(6), finalized)
        .await
        .expect("Timeout waiting for set_keys to finalize")
        .expect("set_keys finalization failed");

    // Clean up
    poll_task.abort();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "This test covers a full E2E flow with multiple steps, and breaking it up would not improve readability"
)]
async fn test_sequencer_checkpoint_resume() {
    // Setup network with faster block production
    let validators = spawn_validators(
        Some("test_sequencer_checkpoint_resume"),
        2,
        |mut config| {
            config.deployment.time.slot_duration = Duration::from_secs(1);
            config
                .user
                .cryptarchia
                .service
                .bootstrap
                .prolonged_bootstrap_period = Duration::ZERO;
            config.deployment.cryptarchia.security_param = NonZero::new(5).unwrap();
            config
        },
        1,
    )
    .await;
    let validator = &validators[0];
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
        let result = handle
            .publish_message(data.clone())
            .await
            .expect("publish failed");
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
        handle
            .publish_message(data.clone())
            .await
            .expect("publish failed");
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
    let mut received: Vec<Vec<u8>> = Vec::new();
    let mut last_zone_block = None;

    let start = std::time::Instant::now();
    let timeout = Duration::from_mins(6);

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
                received.push(block.data.clone());
                last_zone_block = Some((block.id, slot));
            }
        }

        if received.len() >= all_test_data.len() {
            break;
        }

        sleep(Duration::from_millis(500)).await;
    }

    assert_eq!(
        received, all_test_data,
        "Messages should match published order"
    );

    // Clean up
    poll_task.abort();
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
    let validators = spawn_validators(
        Some("test_sequencer_stale_checkpoint_resume"),
        2,
        |mut config| {
            config.deployment.time.slot_duration = Duration::from_secs(1);
            config
                .user
                .cryptarchia
                .service
                .bootstrap
                .prolonged_bootstrap_period = Duration::ZERO;
            config.deployment.cryptarchia.security_param = NonZero::new(5).unwrap();
            config
        },
        1,
    )
    .await;
    let validator = &validators[0];
    let node_url = validator.url();

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
        let r = handle
            .publish_message(data.clone())
            .await
            .expect("publish failed");
        last_result = Some(r);
    }
    let stale_checkpoint = last_result.unwrap().checkpoint;

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
    let phase2_ready_lib_slot = validator.consensus_info(false).await.lib_slot;
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
    let phase3_ready_lib_slot = validator.consensus_info(false).await.lib_slot;
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

    poll_task.abort();
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
        1,
    )
    .await;
    let validator = &validators[0];
    let node_url = validator.url();

    // Random signing key per test run to avoid channel collisions
    let mut key_bytes = [0u8; 32];
    thread_rng().fill(&mut key_bytes);
    let signing_key = Ed25519Key::from_bytes(&key_bytes);
    let channel_id = channel_id_from_key(&signing_key);

    let (sequencer, mut handle) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url.clone()),
        SequencerConfig::default(),
        None, // Fresh start, no checkpoint
    );
    let sequencer_task = sequencer.spawn();
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

    sequencer_task.abort();
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
        1,
    )
    .await;
    let validator = &validators[0];
    let node_url = validator.url();

    // Initialize a sequencer
    // Random signing key per test run to avoid channel collisions
    let mut key_bytes = [0u8; 32];
    thread_rng().fill(&mut key_bytes);
    let signing_key = Ed25519Key::from_bytes(&key_bytes);
    let channel_id = channel_id_from_key(&signing_key);

    let (sequencer, mut handle) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url.clone()),
        SequencerConfig::default(),
        None, // Fresh start, no checkpoint
    );
    let sequencer_task = sequencer.spawn();
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

    sequencer_task.abort();
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
        1,
    )
    .await;
    let validator = &validators[0];
    let node_url = validator.url();

    // Initialize a sequencer
    // Random signing key per test run to avoid channel collisions
    let mut key_bytes = [0u8; 32];
    thread_rng().fill(&mut key_bytes);
    let signing_key = Ed25519Key::from_bytes(&key_bytes);
    let channel_id = channel_id_from_key(&signing_key);

    let (sequencer, mut handle) = ZoneSequencer::init_with_config(
        channel_id,
        signing_key,
        NodeHttpClient::new(CommonHttpClient::new(None), node_url.clone()),
        SequencerConfig::default(),
        None, // Fresh start, no checkpoint
    );
    let sequencer_task = sequencer.spawn();
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
    let withdraw_proof = ChannelWithdrawProof::new(vec![WithdrawSignature::new(
        0,
        handle.sign_tx(&tx).await.unwrap(),
    )])
    .unwrap();

    // Build a signed tx using signatures from user and sequencer
    let signed_tx = SignedMantleTx::new(
        tx,
        vec![
            OpProof::ChannelWithdrawProof(withdraw_proof),
            OpProof::Ed25519Sig(inscription_proof),
        ],
    )
    .unwrap();

    // Submit the signed tx via zone-sdk
    handle.submit_signed_tx(signed_tx, msg_id).await.unwrap();

    // Wait for withdraw/inscription to be finalized and detected by the ZoneIndexer
    wait_for_withdraw(&indexer, &withdraw, Duration::from_mins(2)).await;
    wait_for_zone_block(&indexer, inscription_data, Duration::from_mins(2)).await;

    sequencer_task.abort();
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
    let (configs, genesis_tx) = create_general_configs(count, test_context);
    let genesis_tx =
        add_extra_funding_notes_to_genesis(&configs, genesis_tx, funding_note_values, test_context);
    let deployment_settings = e2e_deployment_settings_with_genesis_tx(genesis_tx);
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

    // Wait for the chain to produce at least one block.
    // Use generous timeout since leader election is probabilistic.
    assert!(wait_for_height(&validators[0], target_block, Duration::from_mins(2)).await);

    validators
}

fn add_extra_funding_notes_to_genesis(
    configs: &[GeneralConfig],
    genesis_tx: GenesisTransaction,
    values: impl IntoIterator<Item = Value>,
    test_context: Option<&str>,
) -> GenesisTransaction {
    let values = values.into_iter().collect::<Vec<_>>();
    if values.is_empty() {
        return genesis_tx;
    }

    let funding_pk = configs[0].consensus_config.funding_pk;
    let mut transfer_op = genesis_tx.genesis_transfer().clone();
    transfer_op
        .outputs
        .extend(values.into_iter().map(|value| Note::new(value, funding_pk)));

    let providers = configs
        .iter()
        .map(|config| {
            let (blend_config, provider_sk, zk_sk) = &config.blend_config;

            ProviderInfo {
                service_type: ServiceType::BlendNetwork,
                provider_sk: provider_sk.clone(),
                zk_sk: zk_sk.clone(),
                locator: Locator(blend_config.core.backend.listening_address.clone()),
                note: config.consensus_config.blend_note.clone(),
            }
        })
        .collect();

    create_genesis_tx_with_declarations(transfer_op, providers, test_context)
}

async fn wait_for_zone_block(
    indexer: &ZoneIndexer<NodeHttpClient>,
    expected_data: Vec<u8>,
    timeout: Duration,
) {
    tokio::time::timeout(timeout, async {
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
    timeout: Duration,
) {
    tokio::time::timeout(timeout, async {
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
    timeout: Duration,
) {
    tokio::time::timeout(timeout, async {
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
