use std::{collections::HashSet, time::Duration};

use lb_common_http_client::CommonHttpClient;
use lb_core::{
    mantle::{
        Note, NoteId, Transaction as _, encoding::MAX_ENCODE_DECODE_INSCRIPTION_SIZE,
        ops::channel::ChannelId,
    },
    sdp::{Declaration, Locator, ServiceType, WithdrawMessage},
};
use lb_key_management_system_service::keys::{Ed25519Key, ZkKey};
use logos_blockchain_tests::{
    common::{
        mantle_tx::{
            create_inscription_transaction_with_id, create_sdp_declare_tx, create_sdp_withdraw_tx,
        },
        time::max_block_propagation_time,
    },
    nodes::validator::Validator,
    topology::{GenesisNoteSpec, Topology, TopologyConfig},
};
use num_bigint::BigUint;
use serial_test::serial;
use tokio::time::{sleep, timeout};

/// High-level SDP flow covered by this E2E:
/// - submit a `Declare` transaction backed by an unused genesis note and wait
///   for inclusion;
/// - advance past the lock period, `Withdraw`, and verify the declaration
///   disappears.
///
/// Note: Activity testing requires the blend service to generate real proofs,
/// which happens automatically for nodes that are declared as blend providers.
/// This test focuses on declare/withdraw flow which doesn't require blend
/// proofs.
#[tokio::test]
#[serial]
#[ignore = "Transaction not being included in blocks - needs investigation"]
async fn sdp_ops_e2e() {
    let note_sk = ZkKey::from(BigUint::from(42u64));
    let spare_note = Note::new(1, note_sk.to_public_key());
    // Use reduced lock_period (3 instead of 10) to speed up the test
    let topology_config = TopologyConfig::two_validators()
        .with_extra_genesis_note(GenesisNoteSpec {
            note: spare_note,
            note_sk: note_sk.clone(),
        })
        .with_lock_period(3);
    let topology = Topology::spawn(topology_config, Some("sdp_ops_e2e")).await;

    topology.wait_network_ready().await;

    let validator = &topology.validators()[0];

    let initial_height_timeout = max_block_propagation_time(
        2, // wait for 1-2 blocks
        2, // network size
        &validator.config().deployment,
        3.5,
    );

    validator
        .wait_for_height(1, initial_height_timeout)
        .await
        .unwrap_or_else(|| panic!("validator should produce the first block before submitting declare- timed out at {initial_height_timeout:.2?}"));

    let inclusion_timeout = Duration::from_secs(30);
    let state_timeout = Duration::from_secs(45);

    let sdp_config = validator.config().deployment.cryptarchia.sdp_config.clone();

    let validator_url = validator.url();
    let client = CommonHttpClient::new(None);

    let existing = validator.get_sdp_declarations().await;
    let locked: HashSet<_> = existing.iter().map(|decl| decl.locked_note_id).collect();

    let injected_note = topology
        .injected_genesis_notes()
        .first()
        .expect("Injected genesis note should exist");

    let locked_note_id = injected_note.note_id;
    assert!(
        !locked.contains(&locked_note_id),
        "Injected note must be unused before submitting declare"
    );

    let provider_signing_key = Ed25519Key::from_bytes(&[7u8; 32]);
    let provider_zk_key = ZkKey::from(BigUint::from(7u64));
    let zk_id = provider_zk_key.to_public_key();
    let locator = Locator(
        "/ip4/127.0.0.1/tcp/9100"
            .parse()
            .expect("Valid locator multiaddr"),
    );

    let (declare_tx, declaration_msg) = create_sdp_declare_tx(
        &provider_signing_key,
        ServiceType::BlendNetwork,
        vec![locator],
        zk_id,
        &provider_zk_key,
        locked_note_id,
        &note_sk,
    );
    let declaration_id = declaration_msg.id();
    let declare_hash = declare_tx.hash();

    client
        .post_transaction(validator_url.clone(), declare_tx)
        .await
        .expect("submit declare transaction");

    let declare_results = validator
        .wait_for_transactions_inclusion(vec![declare_hash], inclusion_timeout)
        .await;

    assert!(
        declare_results.first().is_some_and(Option::is_some),
        "declare transaction should be included"
    );

    let declaration_state = wait_for_declaration(validator, state_timeout, {
        let target_locked_note = locked_note_id;
        move |decl| decl.locked_note_id == target_locked_note
    })
    .await
    .expect("declaration should appear after submission");

    let lock_period = sdp_config
        .service_params
        .get(&ServiceType::BlendNetwork)
        .expect("blend network parameters must exist")
        .lock_period;
    // Use proper timeout calculation based on slot duration and activation
    // coefficient
    let height_timeout = max_block_propagation_time(
        (lock_period + 2) as u32, // lock_period + buffer
        2,                        // network size (2 validators)
        &validator.config().deployment,
        3.0, // margin factor
    );

    let created_height = declaration_state.created;
    let current_nonce = declaration_state.nonce;

    // Wait for chain height to pass the lock period before withdrawing
    validator
        .wait_for_height(created_height + lock_period + 1, height_timeout)
        .await
        .expect("consensus height should pass the SDP lock period");

    // Withdraw requires nonce > declaration's current nonce
    let withdraw_message = WithdrawMessage {
        declaration_id,
        locked_note_id,
        nonce: current_nonce + 1,
    };

    let withdraw_tx = create_sdp_withdraw_tx(withdraw_message, &provider_zk_key, &note_sk);
    let withdraw_hash = withdraw_tx.hash();

    client
        .post_transaction(validator_url, withdraw_tx)
        .await
        .expect("submit withdraw transaction");

    let withdraw_results = validator
        .wait_for_transactions_inclusion(vec![withdraw_hash], inclusion_timeout)
        .await;

    assert!(
        withdraw_results.first().is_some_and(Option::is_some),
        "withdraw transaction should be included"
    );

    let removed = wait_for_declaration_absence(validator, locked_note_id, state_timeout).await;
    assert!(removed, "withdraw should remove the declaration");
}

/// Test that SDP declaration is correctly restored after validator restart.
///
/// This test verifies that after restart, the validator fetches its declaration
/// from the ledger and the SDP service correctly loads declaration state.
#[tokio::test]
#[serial]
async fn sdp_declaration_restoration_e2e() {
    let mut topology = Topology::spawn(
        TopologyConfig::two_validators(),
        Some("sdp_declaration_restoration_e2e"),
    )
    .await;
    topology.wait_network_ready().await;

    let validator = &topology.validators()[0];

    let height_timeout = max_block_propagation_time(
        2, // wait for 1-2 blocks
        2, // network size
        &validator.config().deployment,
        3.5,
    );

    validator
        .wait_for_height(1, height_timeout)
        .await
        .unwrap_or_else(|| {
            panic!("validator should produce the first block - timed out at {height_timeout:.2?}")
        });

    let declarations = validator.get_sdp_declarations().await;
    assert!(
        !declarations.is_empty(),
        "validators should have declarations from genesis"
    );

    let initial_declaration = declarations.first().unwrap().clone();
    let target_locked_note = initial_declaration.locked_note_id;

    let validator = &mut topology.validators_mut()[0];
    validator
        .restart()
        .await
        .expect("validator should restart successfully");

    sleep(Duration::from_secs(5)).await;

    let post_restart_declarations = validator.get_sdp_declarations().await;
    assert!(
        !post_restart_declarations.is_empty(),
        "declarations should be visible after restart"
    );

    let restored_declaration = post_restart_declarations
        .iter()
        .find(|d| d.locked_note_id == target_locked_note)
        .expect("original declaration should still exist after restart");

    assert_eq!(
        restored_declaration.service_type, initial_declaration.service_type,
        "service type should be preserved after restart"
    );
    assert_eq!(
        restored_declaration.zk_id, initial_declaration.zk_id,
        "zk_id should be preserved after restart"
    );

    let logs = validator.get_logs_from_file();
    assert!(
        logs.contains("Loaded declaration from ledger"),
        "SDP service should log that it loaded declaration from ledger"
    );
}

#[tokio::test]
#[serial]
async fn large_inscription_e2e() {
    // The largest payload must leave room for transaction encoding overhead
    // (signatures, headers, etc.) to fit within MAX_BLOCK_SIZE.
    let max_payload = MAX_ENCODE_DECODE_INSCRIPTION_SIZE as usize;
    for payload_size in [
        max_payload / 256,
        max_payload / 64,
        max_payload / 2,
        max_payload,
    ] {
        let topology = Topology::spawn(
            TopologyConfig::two_validators(),
            Some("large_inscription_e2e"),
        )
        .await;
        topology.wait_network_ready().await;

        let validator = &topology.validators()[0];
        let height_timeout = Duration::from_mins(1);
        validator
            .wait_for_height(1, height_timeout)
            .await
            .unwrap_or_else(|| {
                panic!(
                    "validator should produce the first block - timed out at {height_timeout:.2?}"
                )
            });

        let validator_url = validator.url();
        let client = CommonHttpClient::new(None);

        println!("\nTesting inscription with payload size: {payload_size} bytes\n");
        let large_inscription = vec![0xAB; payload_size];
        let mantle_tx = create_inscription_transaction_with_id(
            ChannelId::from([1u8; 32]),
            Some(large_inscription),
        );
        let tx_hash = mantle_tx.hash();

        client
            .post_transaction(validator_url.clone(), mantle_tx)
            .await
            .expect("submit mantle transaction");

        let inclusion_timeout = Duration::from_mins(1);
        let results = validator
            .wait_for_transactions_inclusion(vec![tx_hash], inclusion_timeout)
            .await;

        assert!(
            results.first().is_some_and(Option::is_some),
            "large inscription transaction should be included"
        );
    }
}

async fn wait_for_declaration<F>(
    validator: &Validator,
    duration: Duration,
    predicate: F,
) -> Option<Declaration>
where
    F: Fn(&Declaration) -> bool + Send + Sync + 'static,
{
    timeout(duration, async {
        loop {
            let declarations = validator.get_sdp_declarations().await;
            if let Some(declaration) = declarations.into_iter().find(|decl| predicate(decl)) {
                break declaration;
            }

            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .ok()
}

async fn wait_for_declaration_absence(
    validator: &Validator,
    locked_note_id: NoteId,
    duration: Duration,
) -> bool {
    timeout(duration, async {
        loop {
            let present = validator
                .get_sdp_declarations()
                .await
                .into_iter()
                .any(|decl| decl.locked_note_id == locked_note_id);

            if !present {
                break;
            }

            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .is_ok()
}
