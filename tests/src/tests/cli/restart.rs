use std::time::Duration;

use lb_libp2p::{Multiaddr, Protocol};
use lb_testing_framework::{
    DeploymentBuilder, LbcManualCluster, NodeHttpClient, TopologyConfig as TfTopologyConfig,
};
use logos_blockchain_tests::common::manual_cluster::{
    build_local_manual_cluster, wait_for_height as wait_for_manual_cluster_height,
};
use serial_test::serial;
use testing_framework_core::scenario::{PeerSelection, StartNodeOptions};

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
    );

    let cluster = base.cluster;

    let node0 = cluster
        .start_node_with(
            "0",
            StartNodeOptions::default()
                .with_peers(PeerSelection::None)
                .with_persist_dir(base.scenario_base_dir.join("node-0")),
        )
        .await
        .unwrap_or_else(|_| panic!("starting node-0 should succeed"));

    let node1 = cluster
        .start_node_with(
            "1",
            StartNodeOptions::default()
                .with_peers(PeerSelection::Named(vec![node0.name.clone()]))
                .with_persist_dir(base.scenario_base_dir.join("node-1")),
        )
        .await
        .unwrap_or_else(|_| panic!("starting node-1 should succeed"));

    let node2 = cluster
        .start_node_with(
            "2",
            StartNodeOptions::default()
                .with_peers(PeerSelection::Named(vec![node0.name.clone()]))
                .with_persist_dir(base.scenario_base_dir.join("node-2")),
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
        restart_node_and_get_client(&cluster, &node2.name, vec![node1_dial_addr]).await;

    wait_for_manual_cluster_height(&restarted_node2, 2, Duration::from_mins(2))
        .await
        .expect("node-2 should reach node-1 through the CLI peer override after restart");
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

    cluster
        .restart_node_with(node_name, StartNodeOptions::default().with_args(args))
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
