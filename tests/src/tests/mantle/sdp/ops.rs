use std::{
    collections::{HashMap, HashSet},
    num::NonZero,
    path::PathBuf,
    time::Duration,
};

use lb_core::{
    mantle::{
        GenesisTx as _, MantleTx, NoteId, OpProof, SignedMantleTx, Transaction as _, Utxo,
        genesis_tx::GENESIS_STORAGE_GAS_PRICE,
        ops::Op,
        tx::{GasPrices, MantleTxGasContext},
        tx_builder::MantleTxBuilder,
    },
    sdp::{Declaration, DeclarationMessage, Locator, ServiceType, WithdrawMessage},
};
use lb_key_management_system_service::keys::{Ed25519Key, Ed25519Signature, ZkKey};
use lb_node::config::RunConfig;
use lb_testing_framework::{
    DeploymentBuilder, LbcManualCluster, NodeHttpClient, TopologyConfig as TfTopologyConfig,
    configs::wallet::{WalletAccount, WalletConfig},
};
use logos_blockchain_tests::common::{
    chain::wait_for_transactions_inclusion,
    manual_cluster::{
        build_local_manual_cluster, read_manual_node_logs,
        wait_for_height as wait_for_manual_cluster_height,
    },
    wallet::{
        current_utxos_for_public_key, fund_transfer_builder_from_utxos, utxos_for_public_key,
    },
};
use num_bigint::BigUint;
use testing_framework_core::scenario::{DynError, StartNodeOptions};
use tokio::time::{sleep, timeout};

const LOCK_PERIOD: u64 = 3;

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
#[expect(
    clippy::large_futures,
    reason = "Manual-cluster startup futures are large in these integration tests; boxing would not improve readability"
)]
#[expect(
    clippy::too_many_lines,
    reason = "This test covers a full E2E flow with multiple steps, and breaking it up would not improve readability"
)]
async fn sdp_ops_e2e() {
    let (
        _scenario_base_dir,
        _cluster,
        _node0_name,
        node0,
        genesis_utxos,
        funding_secret_key,
        spare_note_secret_key,
        spare_note_id,
        lock_period,
    ) = start_sdp_manual_cluster("sdp-ops").await;

    let inclusion_timeout = Duration::from_mins(1);
    let state_timeout = Duration::from_secs(45);

    let existing = wait_for_sdp_declarations(&node0, Duration::from_secs(30))
        .await
        .expect("fetching SDP declarations should succeed");
    let locked: HashSet<_> = existing.iter().map(|decl| decl.locked_note_id).collect();
    let locked_note_id = spare_note_id;
    assert!(
        !locked.contains(&locked_note_id),
        "manual-cluster wallet note must be unused before submitting declare"
    );

    let provider_signing_key = Ed25519Key::from_bytes(&[7u8; 32]);
    let provider_zk_key = ZkKey::from(BigUint::from(7u64));
    let zk_id = provider_zk_key.to_public_key();
    let locator = Locator(
        "/ip4/127.0.0.1/tcp/9100"
            .parse()
            .expect("Valid locator multiaddr"),
    );

    let declaration = DeclarationMessage {
        service_type: ServiceType::BlendNetwork,
        locators: vec![locator],
        provider_id: lb_core::sdp::ProviderId::try_from(
            provider_signing_key.public_key().to_bytes(),
        )
        .expect("provider signing key should yield a provider id"),
        zk_id,
        locked_note_id,
    };
    let declaration_id = declaration.id();

    let (declare_mantle_tx, declare_signing_keys) = fund_sdp_transaction(
        &node0,
        &genesis_utxos,
        &funding_secret_key,
        Op::SDPDeclare(declaration),
    )
    .await;
    let declare_hash = declare_mantle_tx.hash();
    let declare_ed25519_sig = Ed25519Signature::from_bytes(
        &provider_signing_key
            .sign_payload(declare_hash.as_signing_bytes().as_ref())
            .to_bytes(),
    );
    let declare_zk_sig = ZkKey::multi_sign(
        &[spare_note_secret_key.clone(), provider_zk_key.clone()],
        &declare_hash.to_fr(),
    )
    .expect("SDP declare zk proof should build");
    let declare_transfer_proof = OpProof::ZkSig(
        ZkKey::multi_sign(&declare_signing_keys, &declare_hash.to_fr())
            .expect("transfer proof should build"),
    );
    let declare_tx = SignedMantleTx::new(
        declare_mantle_tx,
        vec![
            OpProof::ZkAndEd25519Sigs {
                zk_sig: declare_zk_sig,
                ed25519_sig: declare_ed25519_sig,
            },
            declare_transfer_proof,
        ],
    )
    .expect("funded SDP declare transaction should be valid");

    node0
        .submit_transaction(&declare_tx)
        .await
        .expect("submit declare transaction");

    let declare_included =
        wait_for_transactions_inclusion(&node0, &[declare_hash], inclusion_timeout).await;

    assert!(declare_included, "declare transaction should be included");

    let declaration_state = wait_for_declaration(&node0, state_timeout, {
        let target_locked_note = locked_note_id;
        move |decl| decl.locked_note_id == target_locked_note
    })
    .await
    .expect("declaration should appear after submission");

    let created_height = declaration_state.created;
    let current_nonce = declaration_state.nonce;

    wait_for_manual_cluster_height(
        &node0,
        created_height + lock_period + 1,
        Duration::from_mins(2),
    )
    .await
    .expect("consensus height should pass the SDP lock period");

    let withdraw_message = WithdrawMessage {
        declaration_id,
        locked_note_id,
        nonce: current_nonce + 1,
    };

    let (withdraw_mantle_tx, withdraw_signing_keys) = fund_sdp_transaction(
        &node0,
        &genesis_utxos,
        &funding_secret_key,
        Op::SDPWithdraw(withdraw_message),
    )
    .await;

    let withdraw_hash = withdraw_mantle_tx.hash();
    let withdraw_zk_sig = ZkKey::multi_sign(
        &[spare_note_secret_key.clone(), provider_zk_key.clone()],
        &withdraw_hash.to_fr(),
    )
    .expect("SDP withdraw zk proof should build");

    let withdraw_transfer_proof = OpProof::ZkSig(
        ZkKey::multi_sign(&withdraw_signing_keys, &withdraw_hash.to_fr())
            .expect("transfer proof should build"),
    );

    let withdraw_tx = SignedMantleTx::new(
        withdraw_mantle_tx,
        vec![OpProof::ZkSig(withdraw_zk_sig), withdraw_transfer_proof],
    )
    .expect("funded SDP withdraw transaction should be valid");

    node0
        .submit_transaction(&withdraw_tx)
        .await
        .expect("submit withdraw transaction");

    assert!(
        wait_for_transactions_inclusion(&node0, &[withdraw_hash], inclusion_timeout).await,
        "withdraw transaction should be included"
    );

    let removed = wait_for_declaration_absence(&node0, locked_note_id, state_timeout).await;
    assert!(removed, "withdraw should remove the declaration");
}

/// Test that SDP declaration is correctly restored after validator restart.
///
/// This test verifies that after restart, the validator fetches its declaration
/// from the ledger and the SDP service correctly loads declaration state.
#[tokio::test]
#[expect(
    clippy::large_futures,
    reason = "Manual-cluster startup futures are large in these integration tests; boxing would not improve readability"
)]
async fn sdp_declaration_restoration_e2e() {
    let (scenario_base_dir, cluster, node0_name, node0, ..) =
        start_sdp_manual_cluster("sdp-declaration-restoration").await;

    let declarations = node0
        .get_sdp_declarations()
        .await
        .expect("fetching SDP declarations should succeed");
    assert!(
        !declarations.is_empty(),
        "validators should have declarations from genesis"
    );

    let initial_declaration = declarations.first().unwrap().clone();
    let target_locked_note = initial_declaration.locked_note_id;

    cluster
        .restart_node(&node0_name)
        .await
        .expect("manual cluster node should restart successfully");

    sleep(Duration::from_secs(5)).await;

    let post_restart_declarations = cluster
        .node_client(&node0_name)
        .expect("restarted node client should be available")
        .get_sdp_declarations()
        .await
        .expect("fetching post-restart SDP declarations should succeed");
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

    let logs = read_manual_node_logs(&scenario_base_dir, &node0_name);
    assert!(
        logs.contains("Loaded declaration from ledger"),
        "SDP service should log that it loaded declaration from ledger"
    );
}

async fn wait_for_declaration<F>(
    node: &NodeHttpClient,
    duration: Duration,
    predicate: F,
) -> Option<Declaration>
where
    F: Fn(&Declaration) -> bool + Send + Sync + 'static,
{
    timeout(duration, async {
        loop {
            if let Ok(declarations) = node.get_sdp_declarations().await
                && let Some(declaration) = declarations.into_iter().find(|decl| predicate(decl))
            {
                break declaration;
            }

            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .ok()
}

async fn wait_for_declaration_absence(
    node: &NodeHttpClient,
    locked_note_id: NoteId,
    duration: Duration,
) -> bool {
    timeout(duration, async {
        loop {
            let present = node
                .get_sdp_declarations()
                .await
                .map_or(true, |declarations| {
                    declarations
                        .into_iter()
                        .any(|decl| decl.locked_note_id == locked_note_id)
                });

            if !present {
                break;
            }

            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .is_ok()
}

async fn wait_for_sdp_declarations(
    node: &NodeHttpClient,
    duration: Duration,
) -> Option<Vec<Declaration>> {
    timeout(duration, async {
        loop {
            if let Ok(declarations) = node.get_sdp_declarations().await {
                break declarations;
            }

            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .ok()
}

#[expect(
    clippy::large_futures,
    reason = "Manual-cluster startup futures are large in this integration-test helper; boxing would not improve readability"
)]
async fn start_sdp_manual_cluster(
    test_name: &str,
) -> (
    PathBuf,
    LbcManualCluster,
    String,
    NodeHttpClient,
    Vec<Utxo>,
    ZkKey,
    ZkKey,
    NoteId,
    u64,
) {
    let funding_wallet =
        WalletAccount::deterministic(0, 2_000_000, false).expect("funding wallet should build");

    let spare_wallet =
        WalletAccount::deterministic(1, 100, false).expect("spare locked-note wallet should build");

    let base = build_local_manual_cluster(
        test_name,
        "tf-sdp",
        DeploymentBuilder::new(TfTopologyConfig::with_node_numbers(1))
            .with_wallet_config(WalletConfig::new(vec![
                funding_wallet.clone(),
                spare_wallet.clone(),
            ]))
            .with_test_context(test_name),
    );

    let cluster = base.cluster;
    let node0_persist_dir = base.scenario_base_dir.join("node-0");

    let node0 = cluster
        .start_node_with(
            "0",
            StartNodeOptions::default()
                .with_persist_dir(node0_persist_dir)
                .create_patch(|config| Ok::<_, DynError>(patch_sdp_manual_cluster_config(config))),
        )
        .await
        .expect("starting node-0 should succeed");

    cluster
        .wait_network_ready()
        .await
        .expect("manual cluster should become ready");

    wait_for_manual_cluster_height(&node0.client, 1, Duration::from_mins(2))
        .await
        .expect("node-0 should produce the first block");

    let genesis_utxos: Vec<_> = base
        .deployment
        .config
        .genesis_block
        .clone()
        .expect("manual-cluster deployment should include genesis tx")
        .genesis_tx()
        .genesis_transfer()
        .outputs
        .utxos(
            base.deployment
                .config
                .genesis_block
                .expect("manual-cluster deployment should include genesis tx")
                .genesis_tx()
                .genesis_transfer(),
        )
        .collect();

    let spare_note_id =
        utxos_for_public_key(genesis_utxos.iter().copied(), spare_wallet.public_key())
            .first()
            .copied()
            .expect("wallet-backed spare note should exist at genesis")
            .id();

    (
        base.scenario_base_dir,
        cluster,
        node0.name,
        node0.client,
        genesis_utxos,
        funding_wallet.secret_key,
        spare_wallet.secret_key,
        spare_note_id,
        LOCK_PERIOD,
    )
}

fn patch_sdp_manual_cluster_config(mut config: RunConfig) -> RunConfig {
    config.deployment.time.slot_duration = Duration::from_secs(1);
    config
        .user
        .cryptarchia
        .service
        .bootstrap
        .prolonged_bootstrap_period = Duration::ZERO;
    config.deployment.cryptarchia.security_param = NonZero::new(5).unwrap();
    config
        .deployment
        .cryptarchia
        .sdp_config
        .service_params
        .get_mut(&ServiceType::BlendNetwork)
        .expect("blend network params should exist")
        .lock_period = LOCK_PERIOD;
    config
}

async fn fund_sdp_transaction(
    node: &NodeHttpClient,
    genesis_utxos: &[Utxo],
    funding_secret_key: &ZkKey,
    extra_op: Op,
) -> (MantleTx, Vec<ZkKey>) {
    let funding_public_key = funding_secret_key.to_public_key();
    let funding_utxos = current_utxos_for_public_key(node, genesis_utxos, funding_public_key).await;

    let empty_context = MantleTxGasContext::new(
        HashMap::new(),
        GasPrices {
            execution_base_gas_price: 0.into(),
            storage_gas_price: GENESIS_STORAGE_GAS_PRICE,
        },
    );
    let tx_context = lb_core::mantle::tx::MantleTxContext {
        gas_context: empty_context,
        leader_reward_amount: 0,
    };
    let tx_builder = MantleTxBuilder::new(tx_context).push_op(extra_op);

    let funded_builder =
        fund_transfer_builder_from_utxos(funding_utxos, &tx_builder, funding_public_key)
            .expect("funding mixed-op transaction should succeed");

    let signing_keys = funded_builder
        .ledger_inputs()
        .iter()
        .map(|_| funding_secret_key.clone())
        .collect::<Vec<_>>();

    (funded_builder.build(), signing_keys)
}
