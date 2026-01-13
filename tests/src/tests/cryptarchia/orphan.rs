use std::{slice, time::Duration};

use futures::stream::{self, StreamExt as _};
use serial_test::serial;
use tests::{
    adjust_timeout,
    common::sync::{format_cryptarhica_info, wait_for_validators_mode_and_height},
    nodes::validator::{Validator, create_validator_config},
    topology::configs::{
        create_general_configs_with_blend_core_subset,
        network::{Libp2pNetworkLayout, NetworkParams},
    },
};

#[tokio::test]
#[serial]
async fn test_orphan_handling() {
    let n_validators = 3;
    let n_initial_validators = 2;
    let min_height = 5;

    let general_configs = create_general_configs_with_blend_core_subset(
        n_validators,
        n_initial_validators,
        &NetworkParams {
            libp2p_network_layout: Libp2pNetworkLayout::Full,
        },
    );

    let mut validators = vec![];
    for config in general_configs.iter().take(n_initial_validators) {
        let config = create_validator_config(config.clone());
        validators.push(Validator::spawn(config).await.unwrap());
    }
    println!("Initial validators started: {}", validators.len());

    // We set a timeout long enough, since we create multiple UTXOs for each node,
    // but only one of them would be eligible for leadership.
    wait_for_validators_mode_and_height(
        &validators,
        cryptarchia_engine::State::Online,
        min_height,
        Duration::from_secs(500),
    )
    .await;

    // Start the 3rd node. We don't set IBD peers for the node,
    // so it has to catch up via orphan handling
    println!("Starting 3rd node ...");
    let config = create_validator_config(general_configs[n_initial_validators].clone());
    let behind_node = [Validator::spawn(config).await.unwrap()];

    // Orphan handling will be triggered once one of the initial nodes proposes
    // a new block and it is delivered to the behind node.
    // We set a timeout long enough, since there is a non-zero probability that the
    // behind node also proposes blocks (which wouldn't trigger orphan handling).
    tokio::time::timeout(adjust_timeout(Duration::from_secs(300)), async {
        loop {
            let initial_nodes_info: Vec<_> = stream::iter(&validators)
                .then(async |n| n.consensus_info(false).await)
                .collect()
                .await;
            println!(
                "Initial nodes: {:?}",
                format_cryptarhica_info(&initial_nodes_info)
            );

            // take min because we don't know which node will be the first to send an orphan
            // block
            let initial_node_min_height = initial_nodes_info
                .iter()
                .map(|info| info.height)
                .min()
                .unwrap();

            let behind_node_info = behind_node[0].consensus_info(true).await;
            println!(
                "Behind node: {:?}",
                format_cryptarhica_info(slice::from_ref(&behind_node_info))
            );

            if behind_node_info.height >= initial_node_min_height - 1 {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .expect("Timeout waiting for behind node to catch up");
}
