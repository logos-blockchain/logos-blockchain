use std::{
    collections::HashMap,
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
    mantle::{Value, transactions::UNITS_PER_LGO},
    sdp::{Declaration, DeclarationId, Locator, ServiceType},
};
use lb_node::config::{
    RunConfig,
    deployment::era::ruleset::v1::{blend::MinimumNetworkSize, cryptarchia::EpochConfig},
};
use lb_sdp_service::DeclarationRequest;
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
    },
    cucumber::defaults::E2E_ARTIFACTS_DIR,
};
use testing_framework_core::scenario::{DynError, StartNodeOptions};
use tokio::time::{sleep, timeout};

/// High-level SDP flow covered by this E2E:
/// - submit a `Declare` transaction backed by an unused genesis note and wait
///   for inclusion;
/// - submit a `Withdraw` transaction, wait for the finalization delay and check
///   that the declaration disappears.
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
async fn sdp_ops_e2e() {
    let (_cluster, _node0_name, node0, min_stake, slots_per_epoch) =
        start_sdp_manual_cluster("sdp-ops").await;

    let inclusion_timeout = Duration::from_mins(1);

    // The node's wallet funds the stake with an exact note and its KMS holds
    // the provider key, so the new declaration reuses node-0's provider and
    // zk keys with another locator. The SDP binds each id to one stored
    // declaration per service, so node-0's genesis declaration is withdrawn
    // and removed first.
    let existing = wait_for_sdp_declarations(&node0, Duration::from_secs(30))
        .await
        .expect("fetching SDP declarations should succeed");
    let (genesis_declaration_id, genesis_declaration) = existing
        .into_iter()
        .next()
        .expect("node-0 should be declared at genesis");
    withdraw_and_wait_for_removal(
        &node0,
        genesis_declaration_id,
        inclusion_timeout,
        slots_per_epoch,
    )
    .await;
    let provider_id = genesis_declaration.provider_id;
    let locator: Locator = "/ip4/127.0.0.1/tcp/9100"
        .parse()
        .expect("Valid locator multiaddr");

    let declaration = DeclarationRequest {
        service_type: ServiceType::BlendNetwork,
        locators: locator.into(),
        provider_id,
        zk_id: genesis_declaration.zk_id,
        amount: min_stake,
    };
    let declaration_id = declaration.id();

    let declare_hash = node0
        .post_declaration(&declaration)
        .await
        .expect("submit declare transaction")
        .tx_id
        .expect("the declaration should be submitted");

    assert!(
        wait_for_transactions_inclusion(&node0, &[declare_hash], inclusion_timeout).await,
        "declare transaction should be included"
    );

    let declaration_created = get_declaration_by_id(&node0, &declaration_id)
        .await
        .expect("API must succeed")
        .expect("declaration should appear after submission");
    assert_eq!(declaration_created.provider_id, provider_id);

    // Submit an withdraw tx immediately.
    withdraw_and_wait_for_removal(&node0, declaration_id, inclusion_timeout, slots_per_epoch).await;
}

async fn withdraw_and_wait_for_removal(
    node: &NodeHttpClient,
    declaration_id: DeclarationId,
    inclusion_timeout: Duration,
    slots_per_epoch: u64,
) {
    // The node's wallet spends the service note.
    let withdraw_hash = node
        .post_withdrawal(declaration_id)
        .await
        .expect("submit withdraw transaction")
        .tx_id
        .expect("the withdrawal should be submitted");

    assert!(
        wait_for_transactions_inclusion(node, &[withdraw_hash], inclusion_timeout).await,
        "withdraw transaction should be included"
    );

    let withdraw_epoch = get_declaration_by_id(node, &declaration_id)
        .await
        .expect("API must succeed")
        .expect("declaration must still exist until the snapshot finalization delay has passed")
        .withdraw_at
        .expect("withdraw_at must be set after withdraw tx is accepted");

    // Wait until the `withdraw_at + 1` epoch has started: in its first block
    // the service note is unlocked and the declaration is removed.
    wait_for_tip_slot(
        node,
        (u64::from(withdraw_epoch.strict_add(Epoch::new(2)).into_inner()) * slots_per_epoch).into(),
        Duration::from_mins(3),
    )
    .await
    .expect("timed out to wait until the snapshot finalization delay passes after withdraw");

    // Check that the declaration has been removed
    assert!(
        !node
            .get_sdp_declarations()
            .await
            .unwrap()
            .contains_key(&declaration_id)
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

    let initial_declaration = declarations.values().next().unwrap().clone();
    let target_service_note = initial_declaration.service_note;

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
        .values()
        .find(|d| d.service_note == target_service_note)
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

async fn get_declaration_by_id(
    node: &NodeHttpClient,
    declaration_id: &DeclarationId,
) -> Result<Option<Declaration>, Error> {
    Ok(node.get_sdp_declarations().await?.remove(declaration_id))
}

async fn wait_for_sdp_declarations(
    node: &NodeHttpClient,
    duration: Duration,
) -> Option<HashMap<DeclarationId, Declaration>> {
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
    Value,
    u64,
) {
    let slots_per_epoch = Arc::new(AtomicU64::new(0));
    let min_stake = Arc::new(AtomicU64::new(0));
    let funding_wallet = WalletAccount::deterministic(0, 2_000_000 * UNITS_PER_LGO, false)
        .expect("funding wallet should build");

    let spare_wallet = WalletAccount::deterministic(1, 100, false)
        .expect("spare service-note wallet should build");

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
            "node-0",
            StartNodeOptions::default()
                .with_persist_dir(node0_persist_dir)
                .create_patch({
                    let slots_per_epoch = Arc::clone(&slots_per_epoch);
                    let min_stake = Arc::clone(&min_stake);
                    move |config| {
                        let config = patch_sdp_manual_cluster_config(config);
                        slots_per_epoch.store(
                            config
                                .deployment
                                .genesis_era_parameters()
                                .cryptarchia
                                .slots_per_epoch(),
                            Ordering::Relaxed,
                        );
                        min_stake.store(
                            config
                                .deployment
                                .genesis_era_parameters()
                                .cryptarchia
                                .sdp_config
                                .min_stake
                                .threshold,
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

    let node0_name = node0.name;
    let node0_client = node0.client;

    (
        cluster_harness,
        node0_name,
        node0_client,
        min_stake.load(Ordering::Relaxed),
        slots_per_epoch.load(Ordering::Relaxed),
    )
}

fn patch_sdp_manual_cluster_config(mut config: RunConfig) -> RunConfig {
    config
        .user
        .cryptarchia
        .service
        .bootstrap
        .prolonged_bootstrap_period = Duration::ZERO;
    config
        .deployment
        .update_genesis_era_parameters(|parameters| {
            parameters.time.slot_duration_in_seconds = 1.try_into().unwrap();
            parameters.cryptarchia.security_param = NonZero::new(2).unwrap();
            parameters.cryptarchia.slot_activation_coeff =
                NonNegativeRatio::new(1, 2.try_into().unwrap());
            parameters.cryptarchia.epoch_config = EpochConfig {
                epoch_stake_distribution_stabilization: 1.try_into().unwrap(),
                epoch_period_nonce_buffer: 1.try_into().unwrap(),
                epoch_period_nonce_stabilization: 1.try_into().unwrap(),
            };
            parameters.cryptarchia.learning_rate = 0.5.try_into().unwrap();

            let service_params = parameters
                .cryptarchia
                .sdp_config
                .service_params
                .get_mut(&ServiceType::BlendNetwork)
                .expect("blend network params should exist");
            service_params.inactivity_period = 10.try_into().unwrap();

            parameters.blend.common.num_blend_layers = 1.try_into().unwrap();
            parameters.blend.common.minimum_network_size = MinimumNetworkSize::try_new(2).unwrap();
            parameters
                .blend
                .core
                .scheduler
                .delayer
                .maximum_release_delay_in_rounds = 1.try_into().unwrap();
        });

    config
}
