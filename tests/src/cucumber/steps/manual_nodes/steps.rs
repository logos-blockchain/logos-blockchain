use std::time::Duration;

use cucumber::{gherkin::Step, given, then, when};
use lb_testing_framework::{
    DeploymentBuilder, LbcLocalDeployer, TopologyConfig, configs::wallet::WalletAccount,
};
use tokio::time::{Instant, sleep};
use tracing::{info, warn};

use crate::cucumber::{
    error::{StepError, StepResult},
    steps::{TARGET, manual_nodes},
    world::CucumberWorld,
};

#[given(expr = "I have a cluster with capacity of {int} nodes")]
#[when(expr = "I have a cluster with capacity of {int} nodes")]
fn step_manual_cluster(world: &mut CucumberWorld, step: &Step, nodes_count: usize) -> StepResult {
    let mut config = TopologyConfig::with_node_numbers(nodes_count)
        .with_allow_multiple_genesis_tokens(true)
        .with_allow_zero_value_genesis_tokens(true);

    for genesis_token in &world.genesis_tokens {
        let wallet_account = WalletAccount::deterministic(
            genesis_token.account_index as u64,
            genesis_token.token_amount,
            true,
        )?;
        world
            .wallet_accounts
            .insert(genesis_token.account_index, wallet_account.clone());
        for _ in 0..genesis_token.token_count {
            config.wallet_config.accounts.push(wallet_account.clone());
        }
    }

    let deployment = match DeploymentBuilder::new(config).build() {
        Ok(deployment) => deployment,
        Err(e) => {
            warn!(target: TARGET, "Step '{step}' error: {e}");
            return Err(StepError::LogicalError {
                message: format!("failed to build manual cluster: {e}"),
            });
        }
    };
    if let Some(genesis_tx) = deployment.config.genesis_tx.clone() {
        world.genesis_block_utxos = manual_nodes::utils::genesis_block_utxos(&genesis_tx);
    }
    let deployer = LbcLocalDeployer::new();
    let cluster = deployer.manual_cluster_from_descriptors(deployment);
    world.local_cluster = Some(cluster);

    Ok(())
}

#[given(expr = "I start node {string}")]
#[when(expr = "I start node {string}")]
async fn step_start_manual_stand_alone_node(
    world: &mut CucumberWorld,
    step: &Step,
    node_name: String,
) -> StepResult {
    manual_nodes::utils::start_node(world, &step.value, &node_name, &Vec::new(), &Vec::new()).await
}

#[given(expr = "we use IBD peers")]
#[when(expr = "we use IBD peers")]
const fn step_we_use_ibd_peers(world: &mut CucumberWorld) {
    world.populate_ibd_peers = Some(true);
}

#[given(expr = "all peers must be mode online after startup")]
#[when(expr = "all peers must be mode online after startup")]
const fn step_all_nodes_to_br_mode_online(world: &mut CucumberWorld) {
    world.require_all_peers_mode_online_at_startup = Some(true);
}

#[given(expr = "I start peer node {string} connected to node {string}")]
#[when(expr = "I start peer node {string} connected to node {string}")]
async fn step_start_manual_connected_node(
    world: &mut CucumberWorld,
    step: &Step,
    node_name: String,
    peer_name: String,
) -> StepResult {
    manual_nodes::utils::start_node(world, &step.value, &node_name, &Vec::new(), &[peer_name]).await
}

#[given(expr = "I start peer node {string} connected to node {string} and node {string}")]
#[when(expr = "I start peer node {string} connected to node {string} and node {string}")]
async fn step_start_manual_two_connected_nodes(
    world: &mut CucumberWorld,
    step: &Step,
    node_name: String,
    peer_name1: String,
    peer_name2: String,
) -> StepResult {
    manual_nodes::utils::start_node(
        world,
        &step.value,
        &node_name,
        &Vec::new(),
        &[peer_name1, peer_name2],
    )
    .await
}

#[when(expr = "node {string} is at height {int} in {int} seconds")]
#[then(expr = "node {string} is at height {int} in {int} seconds")]
async fn step_node_is_at_height(
    world: &mut CucumberWorld,
    step: &Step,
    node_name: String,
    height: u64,
    time_out_seconds: u64,
) -> StepResult {
    let start = Instant::now();
    let time_out = Duration::from_secs(time_out_seconds);

    let mut count = 0usize;
    loop {
        manual_nodes::utils::poll_all_nodes_and_update_consensus_cache(
            &step.value,
            &mut world.nodes_info,
        )
        .await?;
        let best_height = world.node_best_height(&node_name)?.unwrap_or_default();
        if best_height >= height {
            info!(
                target: TARGET,
                "Node '{node_name}' reached height {height} in {:.2?}",
                start.elapsed()
            );
            return Ok(());
        } else if count.is_multiple_of(50) {
            info!(
                target: TARGET,
                "Waiting for '{node_name}' to reach height {height} - elapsed: {:.2?}, current \
                height: {}", start.elapsed(), best_height
            );
        }

        if start.elapsed() >= time_out {
            return Err(StepError::StepFail {
                message: format!(
                    "Step `{}` error: Node '{node_name}' did not reach height {height} in {time_out_seconds} s",
                    step.value
                ),
            });
        }
        sleep(Duration::from_millis(100)).await;
        count += 1;
    }
}

#[when(expr = "all nodes converged to within {int} blocks in {int} seconds")]
#[then(expr = "all nodes converged to within {int} blocks in {int} seconds")]
async fn step_all_nodes_converged(
    world: &mut CucumberWorld,
    step: &Step,
    max_diff_height: u64,
    time_out_seconds: u64,
) -> StepResult {
    manual_nodes::utils::nodes_converged(
        world,
        &step.value,
        None,
        max_diff_height,
        time_out_seconds,
    )
    .await
}

#[when(
    expr = "all nodes have at least {int} blocks and converged to within {int} blocks in {int} seconds"
)]
#[then(
    expr = "all nodes have at least {int} blocks and converged to within {int} blocks in {int} seconds"
)]
async fn step_all_nodes_reached_min_height_and_converged(
    world: &mut CucumberWorld,
    step: &Step,
    min_height: u64,
    max_diff_height: u64,
    time_out_seconds: u64,
) -> StepResult {
    manual_nodes::utils::nodes_converged(
        world,
        &step.value,
        Some(min_height),
        max_diff_height,
        time_out_seconds,
    )
    .await
}

#[then(expr = "I stop all nodes")]
fn step_stop_all_nodes(world: &mut CucumberWorld) -> StepResult {
    let cluster = world
        .local_cluster
        .as_ref()
        .ok_or(StepError::LogicalError {
            message: "No local cluster available".into(),
        })?;
    let node_names: Vec<String> = world.nodes_info.keys().cloned().collect();
    for node_name in node_names {
        info!(target: TARGET, "Stopping node '{node_name}'");
        let _unused = world.nodes_info.remove(&node_name);
    }
    cluster.stop_all();

    Ok(())
}
