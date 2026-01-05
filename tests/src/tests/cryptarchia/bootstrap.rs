use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use futures::stream::{self, StreamExt as _};
use nomos_libp2p::PeerId;
use serial_test::serial;
use tests::{
    adjust_timeout,
    common::sync::{wait_for_validators_mode, wait_for_validators_mode_and_height},
    nodes::validator::{Validator, create_validator_config},
    secret_key_to_peer_id,
    topology::configs::{
        create_general_configs_with_blend_core_subset,
        network::{Libp2pNetworkLayout, NetworkParams},
    },
};

#[tokio::test]
#[serial]
async fn test_ibd_behind_nodes() {
    let n_validators = 3;
    let n_initial_validators = 2;

    let network_params = NetworkParams {
        libp2p_network_layout: Libp2pNetworkLayout::Full,
    };
    let general_configs = create_general_configs_with_blend_core_subset(
        n_validators,
        n_initial_validators,
        &network_params,
    );

    let mut initial_validators = vec![];
    for config in general_configs.iter().take(n_initial_validators) {
        let config = create_validator_config(config.clone());
        initial_validators.push(Validator::spawn(config).await.unwrap());
    }

    println!("Testing IBD while initial validators are still bootstrapping...");

    let initial_peer_ids: HashSet<PeerId> = general_configs
        .iter()
        .take(n_initial_validators)
        .map(|config| secret_key_to_peer_id(config.network_config.backend.swarm.node_key.clone()))
        .collect();

    let minimum_height = 10;
    println!(
        "Waiting for initial validators to switch to online mode and reach height {minimum_height}...",
    );
    wait_for_validators_mode_and_height(
        &initial_validators,
        cryptarchia_engine::State::Online,
        minimum_height,
        adjust_timeout(Duration::from_secs(300)),
    )
    .await;

    println!("Starting a behind node with IBD peers...");

    let mut config = create_validator_config(general_configs[n_initial_validators].clone());
    config.cryptarchia.network.bootstrap.ibd.peers = initial_peer_ids.clone();
    // Shorten the delay to quickly catching up with peers that grow during IBD.
    // e.g. We start a download only for peer1 because two peers have the same tip
    //      at the moment. But, the peer2 may grow faster than peer1 before IBD is
    // done.      So, we want to check peer1's progress frequently with a very
    // short delay.
    config
        .cryptarchia
        .network
        .bootstrap
        .ibd
        .delay_before_new_download = Duration::from_millis(10);
    // Disable the prolonged bootstrap period for the behind node
    // because we want to check the height of the behind node
    // as soon as it finishes IBD.
    // Currently, checking the mode is only one way to check if IBD is done.
    config
        .cryptarchia
        .service
        .bootstrap
        .prolonged_bootstrap_period = Duration::ZERO;

    let behind_node = Validator::spawn(config.clone())
        .await
        .expect("Behind node should start successfully");

    println!("Behind node started, waiting for it to finish IBD and switch to online mode...");
    wait_for_validators_mode(
        &[&behind_node],
        cryptarchia_engine::State::Online,
        adjust_timeout(Duration::from_secs(10)),
    )
    .await;

    // Check if the behind node has caught up to the highest initial validator.
    let height_check_timestamp = Instant::now();
    let heights = stream::iter(&initial_validators)
        .then(async |n| n.consensus_info(false).await.height)
        .collect::<Vec<_>>()
        .await;
    println!("initial validator heights: {heights:?}");

    let max_initial_validator_height = heights
        .iter()
        .max()
        .expect("There should be at least one initial validator");

    let behind_node_info = behind_node.consensus_info(true).await;
    println!("behind node info: {behind_node_info:?}");

    // We spent some time for checking the heights of nodes
    // after the behind node finishes IBD.
    // So, calculate an acceptable height margin for safe comparison.
    let height_margin = acceptable_height_margin(
        config.deployment.time.slot_duration,
        config
            .deployment
            .cryptarchia
            .consensus_config
            .active_slot_coeff,
        height_check_timestamp.elapsed(),
    );

    println!("Checking if the behind node has caught up to the highest initial validator");
    assert!(
        behind_node_info
            .height
            .abs_diff(*max_initial_validator_height)
            <= height_margin,
    );
}

fn acceptable_height_margin(
    slot_duration: Duration,
    active_slot_coeff: f64,
    duration: Duration,
) -> u64 {
    let block_time = calculate_block_time(slot_duration, active_slot_coeff);
    let margin = duration.div_duration_f64(block_time).ceil() as u64;
    println!(
        "Acceptable height margin:{margin} for duration {duration:?} with block time {block_time:?}"
    );
    margin
}

fn calculate_block_time(slot_duration: Duration, active_slot_coeff: f64) -> Duration {
    println!("slot_duration:{slot_duration:?}, active_slot_coeff:{active_slot_coeff:?}");
    slot_duration.div_f64(active_slot_coeff)
}
