use std::{
    collections::{HashMap, HashSet},
    num::NonZero,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use lb_chain_service::Epoch;
use lb_common_http_client::Error;
use lb_core::{
    mantle::{
        GenesisTx as _, MantleTx, NoteId, OpProof, SignedMantleTx, Transaction as _, Utxo,
        genesis_tx::GENESIS_STORAGE_GAS_PRICE,
        ops::Op,
        tx::{GasPrices, MantleTxGasContext},
        tx_builder::MantleTxBuilder,
    },
    sdp::{
        Declaration, DeclarationMessage, Locator, NumberOfEpochs, ProviderId, ServiceType,
        WithdrawMessage,
    },
};
use lb_key_management_system_service::keys::{Ed25519Key, Ed25519Signature, ZkKey};
use lb_node::config::{
    RunConfig, blend::deployment::MinimumNetworkSize, cryptarchia::deployment::EpochConfig,
};
use lb_testing_framework::{
    DeploymentBuilder, NodeHttpClient, TopologyConfig as TfTopologyConfig,
    configs::wallet::{WalletAccount, WalletConfig},
};
use lb_utils::math::NonNegativeRatio;
use logos_blockchain_tests::{
    common::{
        chain::wait_for_transactions_inclusion,
        manual_cluster::{
            LocalManualClusterHarnessBase, build_local_manual_cluster, read_manual_node_logs,
            wait_for_height as wait_for_manual_cluster_height, wait_for_tip_slot,
        },
        wallet::{current_wallet_funding_source, fund_builder_from_wallet_source},
    },
    cucumber::defaults::E2E_ARTIFACTS_DIR,
};
use num_bigint::BigUint;
use testing_framework_core::scenario::{DynError, StartNodeOptions};
use tokio::time::{sleep, timeout};

const RETENTION_PERIOD: NumberOfEpochs = NumberOfEpochs::new(1);

/// High-level SDP flow covered by this E2E:
/// - submit a `Declare` transaction backed by an unused genesis note and wait
///   for inclusion;
/// - submit a `Withdraw` transaction, wait for the finalization delay and the
///   retention period to pass, and check that the declaration disappears.
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
        _cluster,
        _node0_name,
        node0,
        genesis_utxos,
        funding_wallet,
        spare_note_secret_key,
        spare_note_id,
        slots_per_epoch,
    ) = start_sdp_manual_cluster("sdp-ops").await;

    let inclusion_timeout = Duration::from_mins(1);

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
    let provider_id = ProviderId::try_from(provider_signing_key.public_key().to_bytes())
        .expect("provider signing key should yield a provider id");
    let zk_id = provider_zk_key.to_public_key();
    let locator: Locator = "/ip4/127.0.0.1/tcp/9100"
        .parse()
        .expect("Valid locator multiaddr");

    let declaration = DeclarationMessage {
        service_type: ServiceType::BlendNetwork,
        locators: locator.into(),
        provider_id,
        zk_id,
        locked_note_id,
    };
    let declaration_id = declaration.id();

    let (declare_mantle_tx, declare_signing_keys) = fund_sdp_transaction(
        &node0,
        &genesis_utxos,
        &funding_wallet,
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

    let declaration_created = get_declaration(&node0, &provider_id)
        .await
        .expect("API must succeed")
        .expect("declaration should appear after submission");

    // Submit an withdraw tx immediately.
    let withdraw_message = WithdrawMessage {
        declaration_id,
        locked_note_id,
        nonce: declaration_created.nonce + 1,
    };

    let (withdraw_mantle_tx, withdraw_signing_keys) = fund_sdp_transaction(
        &node0,
        &genesis_utxos,
        &funding_wallet,
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

    let withdraw_epoch = get_declaration(&node0, &provider_id)
        .await
        .expect("API must succeed")
        .expect("declaration must still exist even after withdrawal because GC shouldn't remove it immediately")
        .withdraw_at
        .expect("withdraw_at must be set after withdraw tx is accepted");

    // Wait for the snapshot finalization delay and the retention period to pass.
    wait_for_tip_slot(
        &node0,
        (u64::from((withdraw_epoch.strict_add(RETENTION_PERIOD).strict_add(Epoch::new(1))).into_inner())
            * slots_per_epoch)
            .into(),
        Duration::from_mins(3),
    )
    .await
    .expect("timed out to wait until the snapshot finalization delay and the retention period pass after withdraw");

    // Check that the declaration has been removed
    assert!(
        !node0
            .get_sdp_declarations()
            .await
            .unwrap()
            .iter()
            .any(|declaration| declaration.provider_id == provider_id)
    );
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
    let (cluster_harness, node0_name, node0, ..) =
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

    cluster_harness
        .cluster()
        .restart_node(&node0_name)
        .await
        .expect("manual cluster node should restart successfully");

    sleep(Duration::from_secs(5)).await;

    let post_restart_declarations = cluster_harness
        .cluster()
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

    let logs = read_manual_node_logs(cluster_harness.scenario_base_dir(), &node0_name);
    assert!(
        logs.contains("Loaded declaration from ledger"),
        "SDP service should log that it loaded declaration from ledger"
    );
}

async fn get_declaration(
    node: &NodeHttpClient,
    provider_id: &ProviderId,
) -> Result<Option<Declaration>, Error> {
    Ok(node
        .get_sdp_declarations()
        .await?
        .into_iter()
        .find(|declaration| &declaration.provider_id == provider_id))
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
    LocalManualClusterHarnessBase,
    String,
    NodeHttpClient,
    Vec<Utxo>,
    WalletAccount,
    ZkKey,
    NoteId,
    u64,
) {
    let slots_per_epoch = Arc::new(AtomicU64::new(0));
    let funding_wallet =
        WalletAccount::deterministic(0, 2_000_000, false).expect("funding wallet should build");

    let spare_wallet =
        WalletAccount::deterministic(1, 100, false).expect("spare locked-note wallet should build");

    let cluster_harness = build_local_manual_cluster(
        test_name,
        "tf-sdp",
        DeploymentBuilder::new(TfTopologyConfig::with_node_numbers(1))
            .with_wallet_config(WalletConfig::new(vec![
                funding_wallet.clone(),
                spare_wallet.clone(),
            ]))
            .with_test_context(test_name),
        Some(PathBuf::from(E2E_ARTIFACTS_DIR)),
    );

    let node0_persist_dir = cluster_harness.scenario_base_dir().join("node-0");

    let node0 = cluster_harness
        .cluster()
        .start_node_with(
            "0",
            StartNodeOptions::default()
                .with_persist_dir(node0_persist_dir)
                .create_patch({
                    let slots_per_epoch = Arc::clone(&slots_per_epoch);
                    move |config| {
                        let config = patch_sdp_manual_cluster_config(config);
                        slots_per_epoch.store(
                            config.deployment.cryptarchia.slots_per_epoch(),
                            Ordering::Relaxed,
                        );
                        Ok::<_, DynError>(config)
                    }
                }),
        )
        .await
        .expect("starting node-0 should succeed");

    cluster_harness
        .cluster()
        .wait_network_ready()
        .await
        .expect("manual cluster should become ready");

    wait_for_manual_cluster_height(&node0.client, 1, Duration::from_mins(2))
        .await
        .expect("node-0 should produce the first block");

    let genesis_utxos: Vec<_> = cluster_harness
        .deployment()
        .config
        .genesis_block
        .clone()
        .expect("manual-cluster deployment should include genesis tx")
        .genesis_tx()
        .genesis_transfer()
        .outputs
        .utxos(
            cluster_harness
                .deployment()
                .config
                .genesis_block
                .as_ref()
                .expect("manual-cluster deployment should include genesis tx")
                .genesis_tx()
                .genesis_transfer(),
        )
        .collect();

    let spare_note_id = genesis_utxos
        .iter()
        .copied()
        .find(|utxo| utxo.note.pk == spare_wallet.public_key())
        .expect("wallet-backed spare note should exist at genesis")
        .id();

    let node0_name = node0.name;
    let node0_client = node0.client;

    (
        cluster_harness,
        node0_name,
        node0_client,
        genesis_utxos,
        funding_wallet,
        spare_wallet.secret_key,
        spare_note_id,
        slots_per_epoch.load(Ordering::Relaxed),
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
    config.deployment.cryptarchia.security_param = NonZero::new(2).unwrap();
    config.deployment.cryptarchia.slot_activation_coeff =
        NonNegativeRatio::new(1, 2.try_into().unwrap());
    config.deployment.cryptarchia.epoch_config = EpochConfig {
        epoch_stake_distribution_stabilization: 1.try_into().unwrap(),
        epoch_period_nonce_buffer: 1.try_into().unwrap(),
        epoch_period_nonce_stabilization: 1.try_into().unwrap(),
    };
    config.deployment.cryptarchia.learning_rate = 0.5.try_into().unwrap();

    let service_params = config
        .deployment
        .cryptarchia
        .sdp_config
        .service_params
        .get_mut(&ServiceType::BlendNetwork)
        .expect("blend network params should exist");
    service_params.inactivity_period = 10.try_into().unwrap();
    service_params.retention_period = RETENTION_PERIOD;

    config.deployment.blend.common.num_blend_layers = 1.try_into().unwrap();
    config.deployment.blend.common.minimum_network_size = MinimumNetworkSize::try_new(2).unwrap();
    config
        .deployment
        .blend
        .core
        .scheduler
        .delayer
        .maximum_release_delay_in_rounds = 1.try_into().unwrap();

    config
}

async fn fund_sdp_transaction(
    node: &NodeHttpClient,
    genesis_utxos: &[Utxo],
    funding_wallet: &WalletAccount,
    extra_op: Op,
) -> (MantleTx, Vec<ZkKey>) {
    let funding_source = current_wallet_funding_source(node, genesis_utxos, funding_wallet.clone())
        .await
        .expect("funding wallet source should sync from chain");

    let empty_context = MantleTxGasContext::new(
        HashMap::new(),
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
    let tx_builder = MantleTxBuilder::new(tx_context)
        .push_op(extra_op)
        .expect("mixed-op helper should fit op bounds");

    let funded_builder = fund_builder_from_wallet_source(&funding_source, &tx_builder)
        .expect("funding mixed-op transaction should succeed");

    let signing_keys = funded_builder
        .ledger_inputs()
        .iter()
        .map(|_| funding_wallet.secret_key.clone())
        .collect::<Vec<_>>();

    (
        funded_builder
            .build()
            .expect("funded mixed-op builder should build"),
        signing_keys,
    )
}
