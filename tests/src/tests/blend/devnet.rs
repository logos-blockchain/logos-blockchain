use std::{path::Path, time::Duration};

use lb_libp2p::protocol_name::StreamProtocol;
use lb_node::config::RunConfig;
use lb_testing_framework::{
    DeploymentBuilder, LbcEnv, LbcManualCluster, NodeHttpClient, TopologyConfig as TfTopologyConfig,
};
use logos_blockchain_tests::common::manual_cluster::{
    build_local_manual_cluster, wait_for_height as wait_for_manual_cluster_height,
};
use testing_framework_core::scenario::{DynError, PeerSelection, StartNodeOptions, StartedNode};

const WATCH_PREFIX: &str = "blend-devnet-setup";
const WATCH_INTERVAL: Duration = Duration::from_secs(2);
const DEFAULT_NODE_COUNT: usize = 4;
const NODE_COUNT_ENV: &str = "LOGOS_LOCAL_NODE_COUNT";

// To run use:
// `cargo test -p logos-blockchain-tests --test blend_devnet_setup
// blend_devnet_setup -- --ignored --nocapture`
#[ignore = "For local debugging"]
#[tokio::test]
async fn blend_devnet_setup() {
    let node_count = node_count();
    let base = build_local_manual_cluster(
        "blend-local-watch",
        "tf-local-debug",
        DeploymentBuilder::new(TfTopologyConfig::with_node_numbers(node_count)),
    );

    let cluster = base.cluster;
    let nodes = start_manual_nodes(&cluster, base.scenario_base_dir.as_path(), node_count).await;

    cluster
        .wait_network_ready()
        .await
        .expect("manual cluster should become ready");

    for node in &nodes {
        wait_for_manual_cluster_height(&node.client, 2, Duration::from_mins(3))
            .await
            .unwrap_or_else(|_| panic!("{} should reach height 2", node.name));
    }

    if run_once() {
        print_cluster_snapshot(&nodes).await;
        return;
    }

    loop {
        print_cluster_snapshot(&nodes).await;
        tokio::time::sleep(WATCH_INTERVAL).await;
    }
}

async fn start_manual_nodes(
    cluster: &LbcManualCluster,
    scenario_base_dir: &Path,
    node_count: usize,
) -> Vec<StartedNode<LbcEnv>> {
    let mut nodes = Vec::with_capacity(node_count);

    for index in 0..node_count {
        let peers = peer_selection_for_index(&nodes, index);
        nodes.push(start_manual_node(cluster, &index.to_string(), scenario_base_dir, peers).await);
    }

    nodes
}

async fn start_manual_node(
    cluster: &LbcManualCluster,
    node_name: &str,
    scenario_base_dir: &Path,
    peers: PeerSelection,
) -> StartedNode<LbcEnv> {
    Box::pin(
        cluster.start_node_with(
            node_name,
            StartNodeOptions::default()
                .with_peers(peers)
                .with_persist_dir(scenario_base_dir.join(format!("node-{node_name}")))
                .create_patch(|config| Ok::<_, DynError>(devnet_watch_patch(config))),
        ),
    )
    .await
    .unwrap_or_else(|_| panic!("starting node-{node_name} should succeed"))
}

fn peer_selection_for_index(nodes: &[StartedNode<LbcEnv>], index: usize) -> PeerSelection {
    match index {
        0 => PeerSelection::None,
        1 | 2 => PeerSelection::Named(vec![nodes[0].name.clone()]),
        _ => PeerSelection::Named(nodes.iter().map(|node| node.name.clone()).collect()),
    }
}

fn devnet_watch_patch(mut config: RunConfig) -> RunConfig {
    config
        .user
        .cryptarchia
        .service
        .bootstrap
        .prolonged_bootstrap_period = Duration::ZERO;

    config.deployment.blend.common.protocol_name = StreamProtocol::new("/blend-devnet-setup/blend");
    config.deployment.network.chain_sync_protocol_name =
        StreamProtocol::new("/blend-devnet-setup/chain_sync");
    config.deployment.network.kademlia_protocol_name =
        StreamProtocol::new("/blend-devnet-setup/kademlia");
    config.deployment.network.identify_protocol_name =
        StreamProtocol::new("/blend-devnet-setup/identify");
    config.deployment.cryptarchia.gossipsub_protocol = format!("{WATCH_PREFIX}/gossipsub");
    config.deployment.mempool.pubsub_topic = format!("{WATCH_PREFIX}/mempool");

    config
}

async fn print_cluster_snapshot(nodes: &[StartedNode<LbcEnv>]) {
    for node in nodes {
        print_node_snapshot(&node.name, &node.client).await;
    }
    println!("--------------------------------------------------");
}

async fn print_node_snapshot(node_name: &str, client: &NodeHttpClient) {
    let info = client
        .consensus_info()
        .await
        .expect("fetching consensus info should succeed");

    println!(
        "{node_name} height={} tip={} lib={}",
        info.height, info.tip, info.lib
    );
}

fn run_once() -> bool {
    std::env::var_os("LOGOS_LOCAL_ONCE").is_some()
}

fn node_count() -> usize {
    std::env::var(NODE_COUNT_ENV)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|count| *count > 0)
        .unwrap_or(DEFAULT_NODE_COUNT)
}
