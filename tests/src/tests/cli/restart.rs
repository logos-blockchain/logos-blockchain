use std::time::Duration;

use lb_libp2p::Multiaddr;
use logos_blockchain_tests::{
    common::time::max_block_propagation_time,
    nodes::{Validator, create_validator_config},
    topology::{Topology, TopologyConfig, configs::create_general_configs},
};
use serial_test::serial;

#[tokio::test]
#[serial]
async fn node_restart_w_init_peers() {
    let topology = Topology::spawn(TopologyConfig::two_validators()).await;
    topology.wait_network_ready().await;

    let validator_1 = &topology.validators()[0];
    let validator_1_config = validator_1.config();

    let validator_1_multiaddr: Multiaddr = format!(
        "/ip4/127.0.0.1/udp/{}/quic-v1",
        validator_1_config.user.network.backend.swarm.port
    )
    .try_into()
    .unwrap();

    let validator_2 = &topology.validators()[1];
    let v2_config = validator_2.config();
    let validator_2_multiaddr = format!(
        "/ip4/127.0.0.1/udp/{}/quic-v1",
        v2_config.user.network.backend.swarm.port
    );

    let height_timeout = max_block_propagation_time(2, 3, &validator_1.config().deployment, 3.0);

    validator_1
        .wait_for_height(1, height_timeout)
        .await
        .expect("validator should produce the first block");

    validator_2
        .wait_for_height(2, height_timeout)
        .await
        .expect("validator should produce the first two blocks");

    // Third node that bootstraps from node 1.
    let (mut third_configs, _) = create_general_configs(1);
    let mut third_config = create_validator_config(
        third_configs.remove(0),
        validator_1.config().deployment.clone(),
    );
    third_config.user.network.backend.initial_peers = vec![validator_1_multiaddr];

    let mut third_node = Validator::spawn(third_config)
        .await
        .expect("Spawn third node");

    third_node
        .wait_for_height(1, height_timeout)
        .await
        .expect("validator should produce the first block");

    // Restart third_node with validator_2 as the initial peer.
    // CLI argument is passed to override the previous config.
    third_node
        .restart_with_args(vec![format!(
            "--net-initial-peers={}",
            validator_2_multiaddr
        )])
        .await
        .expect("third_node should restart with validator_2 as peer");

    let sync_timeout = Duration::from_secs(30);
    let target_height = 2;

    // Wait for the restarted third_node to catch up
    third_node
        .wait_for_height(target_height, sync_timeout)
        .await
        .expect("third_node should sync to target height after restart");

    let v1_info = validator_1.consensus_info(true).await;
    let v2_info = validator_2.consensus_info(true).await;
    let v3_info = third_node.consensus_info(true).await;

    println!(
        "Final Heights -> V1: {}, V2: {}, V3: {}",
        v1_info.height, v2_info.height, v3_info.height
    );

    assert!(
        v3_info.height >= target_height,
        "Third node failed to progress after restart"
    );
}
