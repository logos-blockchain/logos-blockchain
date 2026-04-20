use std::collections::HashMap;

use cucumber::{gherkin::Step, given, then, when};
use lb_testing_framework::{K8sManualClusterError, LbcK8sDeployer};
use testing_framework_core::scenario::{PeerSelection, StartNodeOptions};

use crate::cucumber::{
    error::{StepError, StepResult},
    steps::{
        manual_cluster::{
            assert_manual_node_has_peers, build_manual_cluster_deployment, build_user_wallets,
            insert_started_node_info, start_manual_node, stop_active_manual_cluster,
            wait_manual_node_ready,
        },
        manual_nodes::utils::{
            NodesToStartUnordered, parse_wallet_resources_table_row,
            start_nodes_order_respecting_dependencies, verify_node_wallet_resources_table_indexes,
        },
    },
    world::{CucumberWorld, DeployerKind},
};

#[given(expr = "I have a k8s manual cluster with capacity of {int} nodes")]
#[when(expr = "I have a k8s manual cluster with capacity of {int} nodes")]
async fn step_k8s_manual_cluster(world: &mut CucumberWorld, nodes_count: usize) -> StepResult {
    let deployment = build_manual_cluster_deployment(world, nodes_count)?;

    let deployer = LbcK8sDeployer::default();
    let cluster = deployer
        .manual_cluster_from_descriptors(deployment)
        .await
        .map_err(|e| match e {
            K8sManualClusterError::ClientInit { source } => StepError::Preflight {
                message: format!("kubernetes cluster unavailable: {source}"),
            },
            other => StepError::LogicalError {
                message: format!("failed to build k8s manual cluster: {other}"),
            },
        })?;

    world.k8s_manual_cluster = Some(cluster);
    world.set_deployer(DeployerKind::K8s);
    Ok(())
}

#[when(expr = "I k8s-manually start node {string}")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "cucumber step entrypoints must take `&mut World`"
)]
async fn step_k8s_manual_start_node(world: &mut CucumberWorld, node_name: String) -> StepResult {
    start_manual_node(world, &node_name, StartNodeOptions::default()).await?;
    Ok(())
}

#[when(expr = "I k8s-manually start node {string} connected to node {string}")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "cucumber step entrypoints must take `&mut World`"
)]
async fn step_k8s_manual_start_connected_node(
    world: &mut CucumberWorld,
    node_name: String,
    peer_name: String,
) -> StepResult {
    let peer_selection = world.peer_selection_from_names(&[peer_name])?;
    start_manual_node(
        world,
        &node_name,
        StartNodeOptions::default().with_peers(peer_selection),
    )
    .await?;
    Ok(())
}

#[given("I k8s-manually start nodes with wallet resources:")]
#[when("I k8s-manually start nodes with wallet resources:")]
async fn step_k8s_manual_start_nodes_with_wallet_resources(
    world: &mut CucumberWorld,
    step: &Step,
) -> StepResult {
    let table = step.table.as_ref().ok_or(StepError::MissingTable)?;
    verify_node_wallet_resources_table_indexes(table, &step.value)?;

    let mut nodes_to_start: NodesToStartUnordered = HashMap::new();
    for row in table.rows.iter().skip(1) {
        let (node_name, wallet_start_info, connected_to) =
            parse_wallet_resources_table_row(&step.value, row)?;

        let entry = nodes_to_start
            .entry(node_name)
            .or_insert_with(|| (Vec::new(), Vec::new()));
        entry.0.push(wallet_start_info);

        if let Some(peer) = connected_to {
            entry.1.push(peer);
        }
    }

    let ordered = start_nodes_order_respecting_dependencies(nodes_to_start)?;

    for (node_name, wallet_start_info, initial_peers) in ordered {
        let runtime_node_name = format!("node-{}", world.nodes_info.len());
        let started_node = if initial_peers.is_empty() {
            start_manual_node(world, &runtime_node_name, StartNodeOptions::default()).await
        } else {
            let peer_selection = PeerSelection::Named(world.resolve_named_peers(&initial_peers));
            start_manual_node(
                world,
                &runtime_node_name,
                StartNodeOptions::default().with_peers(peer_selection),
            )
            .await
        }
        .map_err(|e| StepError::LogicalError {
            message: format!(
                "failed to start node '{node_name}' as runtime node '{runtime_node_name}': {e}"
            ),
        })?;

        wait_manual_node_ready(world, &runtime_node_name)
            .await
            .map_err(|e| StepError::LogicalError {
                message: format!(
                    "node '{node_name}' (runtime node '{runtime_node_name}') did not become ready: {e}"
                ),
            })?;

        let wallet_info = build_user_wallets(world, &node_name, &wallet_start_info)?;
        insert_started_node_info(world, node_name, started_node, wallet_info);
    }

    let cluster = world
        .k8s_manual_cluster
        .as_ref()
        .ok_or(StepError::LogicalError {
            message: "No k8s manual cluster available".into(),
        })?;
    cluster
        .wait_network_ready()
        .await
        .map_err(|e| StepError::LogicalError {
            message: format!("k8s manual cluster network did not become ready: {e}"),
        })?;

    Ok(())
}

#[then(expr = "k8s manual node {string} has at least {int} peers within {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "cucumber step entrypoints must take `&mut World`"
)]
async fn step_k8s_manual_node_has_peers(
    world: &mut CucumberWorld,
    node_name: String,
    min_peers: usize,
    timeout_secs: u64,
) -> StepResult {
    assert_manual_node_has_peers(world, &node_name, min_peers, timeout_secs).await
}

#[then("I stop all k8s manual nodes")]
fn step_k8s_manual_stop_all_nodes(world: &mut CucumberWorld) -> StepResult {
    stop_active_manual_cluster(world)?;
    world.k8s_manual_cluster = None;
    Ok(())
}
