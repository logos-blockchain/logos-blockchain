use std::{collections::HashSet, time::Duration};

use lb_blend_proofs::{quota::VerifiedProofOfQuota, selection::VerifiedProofOfSelection};
use lb_common_http_client::CommonHttpClient;
use lb_core::{
    mantle::{Note, NoteId, Transaction as _, ops::channel::ChannelId},
    sdp::{
        ActiveMessage, ActivityMetadata, Declaration, Locator, ServiceType, WithdrawMessage,
        blend::ActivityProof as BlendActivityProof,
    },
};
use lb_key_management_system_service::keys::{Ed25519Key, ZkKey};
use logos_blockchain_tests::{
    common::mantle_tx::{
        create_inscription_transaction_with_id, create_sdp_active_tx, create_sdp_declare_tx,
        create_sdp_withdraw_tx,
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
/// - activate the declaration and poll the REST test endpoint until the
///   `Active` height reflects the update;
/// - advance past the lock period,`Withdraw`, and verify the declaration
///   disappears.
#[tokio::test]
#[serial]
#[ignore = "TODO: DA is not supported anymore, rewrite with Blend transaction"]
async fn sdp_ops_e2e() {
    let note_sk = ZkKey::from(BigUint::from(42u64));
    let spare_note = Note::new(1, note_sk.to_public_key());
    let topology_config =
        TopologyConfig::two_validators().with_extra_genesis_note(GenesisNoteSpec {
            note: spare_note,
            note_sk: note_sk.clone(),
        });
    let topology = Topology::spawn(topology_config).await;

    topology.wait_network_ready().await;

    let validator = &topology.validators()[0];

    wait_for_height(validator, 1, Duration::from_secs(30))
        .await
        .expect("validator should produce the first block before submitting declare");

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
    let height_timeout = Duration::from_secs(lock_period.saturating_mul(12).max(90));

    let created_height = declaration_state.created;
    let initial_active = declaration_state.active;
    let mut current_nonce = declaration_state.nonce;

    // Dummy blend metadata for compilation - test is ignored anyway
    // FIXME: provide proper test activity metadata
    let dummy_blend_metadata = ActivityMetadata::Blend(Box::new(BlendActivityProof {
        session: 0,
        signing_key: provider_signing_key.public_key(),
        proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked([0u8; 160]).into(),
        proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked([0u8; 32]).into(),
    }));

    let active_message = ActiveMessage {
        declaration_id,
        nonce: current_nonce + 1,
        metadata: dummy_blend_metadata,
    };

    let active_tx = create_sdp_active_tx(&active_message, &provider_zk_key, &note_sk);

    let active_hash = active_tx.hash();

    client
        .post_transaction(validator_url.clone(), active_tx)
        .await
        .expect("submit active transaction");

    let active_results = validator
        .wait_for_transactions_inclusion(vec![active_hash], inclusion_timeout)
        .await;

    assert!(
        active_results.first().is_some_and(Option::is_some),
        "active transaction should be included"
    );

    current_nonce += 1;

    wait_for_height(validator, created_height + 1, state_timeout)
        .await
        .expect("chain should advance after the active transaction");

    wait_for_declaration(validator, state_timeout, {
        move |decl| decl.locked_note_id == locked_note_id && decl.active > initial_active
    })
    .await
    .expect("Declaration state did not update after active transaction");

    wait_for_height(validator, created_height + lock_period + 1, height_timeout)
        .await
        .expect("consensus height should pass the SDP lock period");

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

#[tokio::test]
#[serial]
async fn large_inscription_e2e() {
    for payload_size in [32 * 1024, 128 * 1024, 512 * 1024, 1024 * 1024] {
        let topology = Topology::spawn(TopologyConfig::two_validators()).await;
        topology.wait_network_ready().await;

        let validator = &topology.validators()[0];
        wait_for_height(validator, 1, Duration::from_secs(30))
            .await
            .expect("validator should produce the first block");

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

        let inclusion_timeout = Duration::from_secs(60);
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

async fn wait_for_height(
    validator: &Validator,
    target_height: u64,
    duration: Duration,
) -> Option<()> {
    timeout(duration, async {
        let mut tick: u8 = 0;
        loop {
            let info = validator.consensus_info(tick == 0).await;
            if info.height >= target_height {
                println!(
                    "waiting for height {}... current height is {}",
                    target_height, info.height
                );
                println!("{info:?}");
                break;
            }
            if tick.is_multiple_of(10) {
                println!(
                    "waiting for height {}... current height is {}",
                    target_height, info.height
                );
            }
            tick = tick.wrapping_add(1);

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .ok()
}
