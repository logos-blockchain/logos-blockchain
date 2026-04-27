use std::time::Duration;

use lb_libp2p::Multiaddr;
use lb_node::config::RunConfig;
use lb_testing_framework::{
    DeploymentBuilder, LbcManualCluster, NodeHttpClient, TopologyConfig as TfTopologyConfig,
};
use logos_blockchain_tests::common::manual_cluster::{
    build_local_manual_cluster, override_node_initial_peers,
    wait_for_height as wait_for_manual_cluster_height,
};
use testing_framework_core::scenario::{DynError, PeerSelection, StartNodeOptions};

#[tokio::test]
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

    let node0_multiaddr = node_multiaddr(&base.deployment, 0);
    let node1_multiaddr = node_multiaddr(&base.deployment, 1);

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
                .with_peers(PeerSelection::None)
                .with_persist_dir(base.scenario_base_dir.join("node-2"))
                .create_patch(move |config| {
                    Ok::<_, DynError>(override_initial_peers(
                        config,
                        vec![node0_multiaddr.clone()],
                    ))
                }),
        )
        .await
        .unwrap_or_else(|_| panic!("starting node-2 should succeed"));

    cluster
        .wait_network_ready()
        .await
        .expect("manual cluster should become ready");

    wait_for_manual_cluster_height(&node0.client, 1, Duration::from_mins(5))
        .await
        .expect("node-0 should produce the first block");

    wait_for_manual_cluster_height(&node1.client, 2, Duration::from_mins(5))
        .await
        .expect("node-1 should reach height 2");

    wait_for_manual_cluster_height(&node2.client, 1, Duration::from_mins(5))
        .await
        .expect("node-2 should bootstrap from node-0");

    let restarted_node2 = restart_node_and_get_client(
        &base.scenario_base_dir,
        &cluster,
        &node2.name,
        vec![node1_multiaddr.clone()],
    )
    .await;

    wait_for_manual_cluster_height(&restarted_node2, 2, Duration::from_mins(2))
        .await
        .expect("node-2 should rejoin and reach height 2 after restart");
}

fn node_multiaddr(
    deployment: &lb_testing_framework::internal::DeploymentPlan,
    node_index: usize,
) -> Multiaddr {
    let port = deployment.nodes()[node_index]
        .general
        .network_config
        .backend
        .swarm
        .port;

    format!("/ip4/127.0.0.1/udp/{port}/quic-v1")
        .parse()
        .expect("node multiaddr should parse")
}

async fn restart_node_and_get_client(
    scenario_base_dir: &std::path::Path,
    cluster: &LbcManualCluster,
    node_name: &str,
    initial_peers: Vec<Multiaddr>,
) -> NodeHttpClient {
    override_node_initial_peers(scenario_base_dir, node_name, initial_peers);

    cluster
        .restart_node(node_name)
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

fn override_initial_peers(mut config: RunConfig, initial_peers: Vec<Multiaddr>) -> RunConfig {
    config.user.network.backend.initial_peers = initial_peers;
    config
}
