use std::{
    future::ready,
    num::{NonZero, NonZeroU64},
    path::{Path, PathBuf},
    time::Duration,
};

use futures::StreamExt as _;
use lb_binary_codec::bincode::DeserializeOp as _;
use lb_chain_service::{
    CryptarchiaInfo, DEFAULT_CHECKPOINT_INTERVAL_BLOCKS, LibCheckpoint, saved_tip_era,
};
use lb_common_http_client::CommonHttpClient;
use lb_core::header::HeaderId;
use lb_http_api_common::queries::{BlockFilter, BlockSortOrder, BlocksStreamQuery};
use lb_libp2p::{Multiaddr, Protocol};
use lb_storage_service::{
    backend::StorageBackend as _,
    recovery::{load_recovery_data, recovery_key},
    rocksdb::{RocksBackend, RocksBackendSettings},
};
use lb_testing_framework::{
    DeploymentBuilder, LbcEnv, LbcManualCluster, LbcNodeConfig, NodeHttpClient,
    TopologyConfig as TfTopologyConfig, USER_CONFIG_FILE, ensure_node_binary_built, node_db_subdir,
};
use lb_utils::math::NonNegativeRatio;
use logos_blockchain_tests::{
    common::manual_cluster::{
        LocalManualClusterHarnessBase, build_local_manual_cluster,
        wait_for_height as wait_for_manual_cluster_height,
    },
    cucumber::defaults::E2E_ARTIFACTS_DIR,
};
use serial_test::serial;
use testing_framework_core::scenario::{PeerSelection, StartNodeOptions, StartedNode};
use tokio::{process::Command, time::timeout};

const RECOVERY_SECURITY_PARAM: u32 = 5;
const RECOVERY_PROGRESS_KEY: &[u8] = b"cryptarchia-progress-v1";
const RECOVERY_CHECKPOINT_KEY: &[u8] = b"cryptarchia";

#[tokio::test]
#[serial]
#[expect(
    clippy::large_futures,
    reason = "Manual-cluster start operations return large futures in tests; boxing adds noise without improving the test"
)]
async fn node_restart_with_initial_peer_override() {
    let base = build_local_manual_cluster(
        "manual-cluster-restart",
        "tf-manual-restart",
        DeploymentBuilder::new(
            TfTopologyConfig::with_node_numbers(3)
                .with_test_context(Some("node_restart_with_initial_peer_override".to_owned())),
        ),
        Some(PathBuf::from(E2E_ARTIFACTS_DIR)),
    );

    let cluster = base.cluster();

    let node0 = cluster
        .start_node_with(
            "0",
            StartNodeOptions::default()
                .with_peers(PeerSelection::None)
                .with_persist_dir(base.scenario_base_dir().join("node-0")),
        )
        .await
        .unwrap_or_else(|_| panic!("starting node-0 should succeed"));

    let node1 = cluster
        .start_node_with(
            "1",
            StartNodeOptions::default()
                .with_peers(PeerSelection::Named(vec![node0.name.clone()]))
                .with_persist_dir(base.scenario_base_dir().join("node-1")),
        )
        .await
        .unwrap_or_else(|_| panic!("starting node-1 should succeed"));

    let node2 = cluster
        .start_node_with(
            "2",
            StartNodeOptions::default()
                .with_peers(PeerSelection::Named(vec![node0.name.clone()]))
                .with_persist_dir(base.scenario_base_dir().join("node-2")),
        )
        .await
        .unwrap_or_else(|_| panic!("starting node-2 should succeed"));

    cluster
        .wait_network_ready()
        .await
        .expect("manual cluster should become ready");

    let node1_dial_addr = dial_addr(&node1.client).await;

    wait_for_manual_cluster_height(&node0.client, 1, Duration::from_mins(5))
        .await
        .expect("node-0 should produce the first block");

    wait_for_manual_cluster_height(&node1.client, 2, Duration::from_mins(5))
        .await
        .expect("node-1 should reach height 2");

    wait_for_manual_cluster_height(&node2.client, 1, Duration::from_mins(5))
        .await
        .expect("node-2 should bootstrap from node-0");

    cluster
        .stop_node(&node0.name)
        .await
        .unwrap_or_else(|_| panic!("node-0 should stop cleanly"));

    let restarted_node2 =
        restart_node_and_get_client(cluster, &node2.name, vec![node1_dial_addr]).await;

    wait_for_manual_cluster_height(&restarted_node2, 2, Duration::from_mins(2))
        .await
        .expect("node-2 should reach node-1 through the CLI peer override after restart");
}

#[tokio::test]
#[serial]
async fn peerless_crash_recovers_canonical_history_after_the_checkpoint() {
    const CHECKPOINT_INTERVAL_BLOCKS: NonZeroU64 = NonZeroU64::new(24).unwrap();
    const BLOCKS_AFTER_ROLLOVER: u64 = 8;

    let (base, node) = start_recovery_fixture(
        "canonical-history-after-checkpoint",
        "peerless_crash_recovers_canonical_history_after_the_checkpoint",
        CHECKPOINT_INTERVAL_BLOCKS,
    )
    .await;
    let cluster = base.cluster();
    let database_settings = recovery_database(base.scenario_base_dir());
    let genesis_checkpoint = {
        let database = RocksBackend::new(database_settings.clone()).unwrap();
        checkpoint_bytes(&database)
    };

    wait_for_manual_cluster_height(
        &node.client,
        CHECKPOINT_INTERVAL_BLOCKS.get() + u64::from(RECOVERY_SECURITY_PARAM),
        Duration::from_mins(6),
    )
    .await
    .expect("the configured number of LIB advances should refresh the full checkpoint");

    let progress_key = recovery_key(RECOVERY_PROGRESS_KEY);
    let (refreshed_checkpoint, checkpoint_progress) = {
        let database = RocksBackend::new(database_settings.clone()).unwrap();
        (
            checkpoint_bytes(&database),
            database.load(&progress_key).unwrap().unwrap(),
        )
    };
    assert!(
        refreshed_checkpoint != genesis_checkpoint,
        "the recorder should have replaced the genesis checkpoint"
    );
    let checkpoint = LibCheckpoint::from_bytes(&refreshed_checkpoint).unwrap();

    let checkpoint_height = node
        .client
        .consensus_info()
        .await
        .unwrap()
        .cryptarchia_info
        .height;
    wait_for_manual_cluster_height(
        &node.client,
        checkpoint_height + BLOCKS_AFTER_ROLLOVER,
        Duration::from_mins(2),
    )
    .await
    .expect("isolated node should build history beyond the refreshed checkpoint");

    let before = node.client.consensus_info().await.unwrap().cryptarchia_info;
    let (genesis, history_before) = canonical_history(&node.client, &before).await;
    assert_eq!(
        LibCheckpoint::from_bytes(&genesis_checkpoint)
            .unwrap()
            .tip(),
        genesis,
        "the original checkpoint should belong to genesis"
    );

    let finalized_index = history_before
        .iter()
        .position(|id| *id == before.lib)
        .expect("the pre-crash LIB should be on the canonical chain");
    assert!(
        history_before[..finalized_index].contains(&checkpoint.tip()),
        "the refreshed checkpoint should precede some pre-crash finalized history"
    );

    let http = CommonHttpClient::new(None);
    let block_events_before = block_events_at_saved_heads(&http, &node.client, &before).await;

    // The local harness kills the process without a graceful shutdown and
    // waits for it to exit. Recovery must use the RocksDB writes already made.
    cluster
        .stop_node(&node.name)
        .await
        .expect("isolated node should exit");

    let progress_len = {
        let database = RocksBackend::new(database_settings).unwrap();
        assert!(
            checkpoint_bytes(&database) == refreshed_checkpoint,
            "further LIB advances below the next threshold should leave the checkpoint unchanged"
        );

        let progress = database
            .load(&progress_key)
            .unwrap()
            .expect("the crash should leave compact recovery progress in RocksDB");
        assert_ne!(
            checkpoint_progress, progress,
            "compact recovery progress should advance beyond the refreshed checkpoint"
        );

        assert_eq!(
            database.get_immutable_block_id(before.lib_slot).unwrap(),
            Some(before.lib)
        );

        progress.len()
    };

    println!(
        "checkpoint {} stayed at {} bytes; updated recovery progress is {progress_len} bytes",
        checkpoint.tip(),
        refreshed_checkpoint.len(),
    );

    Box::pin(cluster.restart_node(&node.name))
        .await
        .expect("isolated node should restart from the same database");
    cluster
        .wait_node_ready(&node.name)
        .await
        .expect("isolated node should become ready after replay");

    let restarted = cluster.node_client(&node.name).unwrap();
    let after = restarted.consensus_info().await.unwrap().cryptarchia_info;
    let (recovered_genesis, history_after) = canonical_history(&restarted, &after).await;

    assert_eq!(recovered_genesis, genesis);
    assert!(
        after.lib_slot >= before.lib_slot,
        "the LIB must never regress"
    );
    assert!(
        history_after.starts_with(&history_before),
        "the recovered canonical chain must contain the exact pre-crash history, including its LIB and tip"
    );

    println!(
        "recovered all {} pre-crash canonical blocks from disk without peers",
        history_before.len()
    );

    assert_recovered_immutable_range(&restarted, &before, &history_before[..=finalized_index])
        .await;

    assert_eq!(
        block_events_at_saved_heads(&http, &restarted, &before).await,
        block_events_before,
        "events for the pre-crash LIB and tip should remain identical"
    );

    assert_live_subscriptions_resume(&http, &restarted, &after).await;
}

#[tokio::test]
#[serial]
async fn corrupt_full_checkpoint_aborts_node_startup() {
    let (base, node) = start_recovery_fixture(
        "corrupt-full-checkpoint",
        "corrupt_full_checkpoint_aborts_node_startup",
        DEFAULT_CHECKPOINT_INTERVAL_BLOCKS,
    )
    .await;
    let cluster = base.cluster();
    cluster
        .stop_node(&node.name)
        .await
        .expect("isolated node should exit");

    let mut database_settings = recovery_database(base.scenario_base_dir());
    let checkpoint_key = recovery_key(RECOVERY_CHECKPOINT_KEY);
    let progress_key = recovery_key(RECOVERY_PROGRESS_KEY);
    let data = load_recovery_data(database_settings.clone()).unwrap();

    let compact = data
        .get(&progress_key)
        .unwrap()
        .expect("compact progress should exist");
    assert!(
        saved_tip_era(&data)
            .expect("compact progress should decode")
            .is_some()
    );

    database_settings.read_only = false;
    let database = RocksBackend::new(database_settings.clone()).unwrap();
    let mut checkpoint = database.load(&checkpoint_key).unwrap().unwrap();
    assert!(LibCheckpoint::from_bytes(&checkpoint).is_ok());

    checkpoint.truncate(1);
    database.store(checkpoint_key.clone(), checkpoint).unwrap();

    let corrupted = database.load(&checkpoint_key).unwrap().unwrap();
    assert!(LibCheckpoint::from_bytes(&corrupted).is_err());
    assert_eq!(database.load(&progress_key).unwrap(), Some(compact));
    drop(database);

    let runtime = database_settings
        .db_path
        .parent()
        .expect("test node has a runtime directory");
    let binary = ensure_node_binary_built(base.deployment().config().node_binary_profile())
        .await
        .expect("resolve the same node binary used by the harness");
    let mut command = Command::new(binary);
    command
        .arg(runtime.join(USER_CONFIG_FILE))
        .arg("--deployment")
        .arg(runtime.join("deployment.yaml"))
        .current_dir(runtime)
        .kill_on_drop(true);

    let rejected = timeout(Duration::from_secs(20), command.output())
        .await
        .expect("node should exit promptly with a corrupt checkpoint")
        .expect("launching the node process should succeed");
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(
        !rejected.status.success(),
        "node startup should fail: {stderr}"
    );
    assert!(
        stderr.contains("The saved chain checkpoint cannot be read"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Remove the node's state and sync again."),
        "{stderr}"
    );
}

async fn start_recovery_fixture(
    scenario: &str,
    test_context: &str,
    checkpoint_interval_blocks: NonZeroU64,
) -> (LocalManualClusterHarnessBase, StartedNode<LbcEnv>) {
    const NODE_NAME: &str = "node";

    let base = build_local_manual_cluster(
        scenario,
        "tf-crash-recovery",
        DeploymentBuilder::new(
            TfTopologyConfig::with_node_numbers(1).with_test_context(Some(test_context.to_owned())),
        ),
        Some(PathBuf::from(E2E_ARTIFACTS_DIR)),
    );
    let cluster = base.cluster();
    let node = Box::pin(
        cluster.start_node_with(
            NODE_NAME,
            StartNodeOptions::default()
                .with_peers(PeerSelection::None)
                .with_persist_dir(base.scenario_base_dir().join(NODE_NAME))
                .create_patch(move |mut config: LbcNodeConfig| {
                    config
                        .deployment
                        .update_genesis_era_parameters(|parameters| {
                            parameters.time.slot_duration_in_seconds = 1.try_into().unwrap();

                            let cryptarchia = &mut parameters.cryptarchia;
                            cryptarchia.security_param =
                                NonZero::new(RECOVERY_SECURITY_PARAM).unwrap();
                            cryptarchia.slot_activation_coeff =
                                NonNegativeRatio::new(9, NonZero::new(10).unwrap())
                                    .try_into()
                                    .expect("9/10 should be a valid slot activation coefficient");
                            cryptarchia.uncle_reference_window_in_block = cryptarchia
                                .uncle_reference_window_in_block
                                .min(cryptarchia.widest_uncle_reference_window());
                        })?;

                    let service = &mut config.user.cryptarchia.service;
                    service.checkpoint_interval_blocks = checkpoint_interval_blocks;

                    let bootstrap = &mut service.bootstrap;
                    bootstrap.prolonged_bootstrap_period = Duration::ZERO;
                    bootstrap.offline_grace_period.grace_period = Duration::ZERO;

                    Ok(config)
                }),
        ),
    )
    .await
    .expect("isolated block producer should start");

    cluster
        .wait_node_ready(&node.name)
        .await
        .expect("isolated block producer should become ready");
    wait_for_manual_cluster_height(&node.client, 1, Duration::from_mins(2))
        .await
        .expect("the first block should trigger recovery persistence");

    (base, node)
}

async fn assert_recovered_immutable_range(
    client: &NodeHttpClient,
    before: &CryptarchiaInfo,
    expected_ids: &[HeaderId],
) {
    let stream = client
        .blocks_range_stream(BlocksStreamQuery {
            slot_from: Some(1),
            slot_to: Some(before.lib_slot.into_inner()),
            order: Some(BlockSortOrder::Ascending),
            blocks_limit: None,
            server_batch_size: NonZero::new(2),
            block_filter: Some(BlockFilter::ImmutableOnly),
        })
        .await
        .expect("the immutable range stream should open after restart");

    let ids = timeout(
        Duration::from_secs(15),
        stream
            .map(|event| event.block.header.id)
            .collect::<Vec<_>>(),
    )
    .await
    .expect("the bounded immutable stream should finish");

    assert_eq!(
        ids, expected_ids,
        "the stream should return the exact finalized history"
    );
}

async fn block_events_at_saved_heads(
    http: &CommonHttpClient,
    client: &NodeHttpClient,
    saved: &CryptarchiaInfo,
) -> [serde_json::Value; 2] {
    let base_url = client.base_url();
    let events: [_; 2] = timeout(Duration::from_secs(15), async {
        tokio::try_join!(
            http.get_block_events(base_url.clone(), saved.lib),
            http.get_block_events(base_url.clone(), saved.tip),
        )
    })
    .await
    .expect("the saved block events requests should finish")
    .expect("the saved block events requests should succeed")
    .into();

    events.map(|events| {
        serde_json::to_value(events.expect("the saved block should have stored events"))
            .expect("block events should serialize")
    })
}

async fn assert_live_subscriptions_resume(
    http: &CommonHttpClient,
    client: &NodeHttpClient,
    restored: &CryptarchiaInfo,
) {
    let base_url = client.base_url();
    let (blocks, finalized) = timeout(Duration::from_secs(15), async {
        tokio::try_join!(
            http.get_blocks_stream(base_url.clone()),
            http.get_lib_stream(base_url.clone()),
        )
    })
    .await
    .expect("both live subscriptions should connect after restart")
    .expect("both live subscriptions should open successfully");
    tokio::pin!(blocks, finalized);

    let (new_block, new_lib) = timeout(Duration::from_secs(30), async {
        tokio::join!(
            async {
                // The first block notification can be the subscription's
                // snapshot.
                let first = blocks
                    .next()
                    .await
                    .expect("the block stream should stay open");
                let slot_floor = first.block.header.slot.max(restored.slot);

                blocks
                    .filter(|event| ready(event.block.header.slot > slot_floor))
                    .next()
                    .await
            },
            finalized.next(),
        )
    })
    .await
    .expect("both subscriptions should deliver new live notifications");

    let new_block = new_block.expect("the block stream should deliver a newly produced block");
    assert_eq!(new_block.tip, new_block.block.header.id);
    assert_eq!(new_block.tip_slot, new_block.block.header.slot);
    assert!(new_block.lib_slot >= restored.lib_slot);

    let new_lib = new_lib.expect("the LIB stream should deliver a new finalization");
    let announced_lib = client
        .block(&new_lib.header_id)
        .await
        .expect("the announced LIB should be queryable")
        .expect("the announced LIB should exist");
    assert!(announced_lib.header.slot > restored.lib_slot);

    println!("immutable history, saved block events, and both live subscriptions verified");
}

fn recovery_database(scenario_dir: &Path) -> RocksBackendSettings {
    let db_path = scenario_dir
        .read_dir()
        .unwrap()
        .map(|entry| entry.unwrap().path().join(node_db_subdir()))
        .find(|candidate| candidate.is_dir())
        .expect("local node runtime directory should contain its RocksDB database");

    RocksBackendSettings {
        db_path,
        read_only: true,
        column_family: Some("blocks".to_owned()),
    }
}

fn checkpoint_bytes(database: &RocksBackend) -> Vec<u8> {
    database
        .load(&recovery_key(RECOVERY_CHECKPOINT_KEY))
        .unwrap()
        .expect("the initial full checkpoint should be present")
        .to_vec()
}

async fn canonical_history(
    client: &NodeHttpClient,
    info: &CryptarchiaInfo,
) -> (HeaderId, Vec<HeaderId>) {
    let mut tip = info.tip;
    let mut history = Vec::new();

    for _ in 0..info.height {
        let block = client
            .block(&tip)
            .await
            .expect("historical block request should succeed")
            .expect("each canonical block should be available by ID");

        assert_eq!(block.header.id, tip);
        history.push(tip);
        tip = block.header.parent_block;
    }

    history.reverse();
    (tip, history)
}

async fn dial_addr(client: &NodeHttpClient) -> Multiaddr {
    let network = client
        .network_info()
        .await
        .expect("node should expose network info");

    let mut addr = network
        .listen_addresses
        .into_iter()
        .next()
        .expect("network info should expose at least one listen address");

    addr.push(Protocol::P2p(network.peer_id));

    addr
}

async fn restart_node_and_get_client(
    cluster: &LbcManualCluster,
    node_name: &str,
    initial_peers: Vec<Multiaddr>,
) -> NodeHttpClient {
    let mut args = vec!["--net-initial-peers".to_owned()];
    args.extend(initial_peers.into_iter().map(|addr| addr.to_string()));

    Box::pin(cluster.restart_node_with(node_name, StartNodeOptions::default().with_args(args)))
        .await
        .unwrap_or_else(|_| panic!("node `{node_name}` should restart"));

    cluster
        .wait_node_ready(node_name)
        .await
        .unwrap_or_else(|_| panic!("node `{node_name}` should become ready after restart"));

    cluster
        .node_client(node_name)
        .unwrap_or_else(|| panic!("node `{node_name}` client should be available after restart"))
}
