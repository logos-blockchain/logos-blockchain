use std::{collections::HashMap, time::Duration};

use cucumber::{given, then, when};
use futures::future::try_join_all;
use hex::ToHex as _;
use lb_testing_framework::{DeploymentBuilder, LbcLocalDeployer, TopologyConfig};
use testing_framework_core::scenario::{PeerSelection, StartNodeOptions};
use tokio::time::{Instant, sleep};
use tracing::{info, warn};

use crate::cucumber::{
    error::{StepError, StepResult},
    steps::TARGET,
    utils::track_progress,
    world::{ChainInfoMap, CucumberWorld, NodeInfo},
};

enum AlignmentStatus {
    MissingChainInfo,
    Fork,
    Aligned,
}

#[derive(Debug, Clone)]
struct ConsensusSnapshot {
    node_name: String,
    height: u64,
    header_hash: String,
}

#[derive(Debug, Clone)]
struct MaybeSnapshot {
    height: u64,
    header_hash: Option<String>,
}

#[given(expr = "I have a cluster with capacity of {int} nodes")]
#[when(expr = "I have a cluster with capacity of {int} nodes")]
fn manual_cluster(world: &mut CucumberWorld, nodes_count: usize) -> StepResult {
    let config = TopologyConfig::with_node_numbers(nodes_count);
    let deployment = match DeploymentBuilder::new(config).build() {
        Ok(deployment) => deployment,
        Err(source) => {
            warn!(
                target: TARGET,
                "Step 'I have a we have a cluster with capacity of {nodes_count} nodes' error: \
                 {source}"
            );
            return Err(StepError::LogicalError {
                message: format!("failed to build manual cluster: {source}"),
            });
        }
    };
    let deployer = LbcLocalDeployer::new();
    let cluster = deployer.manual_cluster_from_descriptors(deployment);
    world.local_cluster = Some(cluster);

    Ok(())
}

#[given(expr = "I start node {string}")]
#[when(expr = "I start node {string}")]
async fn start_manual_stand_alone_node(world: &mut CucumberWorld, node_name: String) -> StepResult {
    start_node(world, node_name, &Vec::new()).await
}

#[given(expr = "I start peer node {string} connected to node {string}")]
#[when(expr = "I start peer node {string} connected to node {string}")]
async fn start_manual_connected_node(
    world: &mut CucumberWorld,
    node_name: String,
    peer_name: String,
) -> StepResult {
    start_node(world, node_name, &[peer_name]).await
}

#[given(expr = "I start peer node {string} connected to node {string} and node {string}")]
#[when(expr = "I start peer node {string} connected to node {string} and node {string}")]
async fn start_manual_two_connected_nodes(
    world: &mut CucumberWorld,
    node_name: String,
    peer_name1: String,
    peer_name2: String,
) -> StepResult {
    start_node(world, node_name, &[peer_name1, peer_name2]).await
}

async fn start_node(world: &mut CucumberWorld, node_name: String, peers: &[String]) -> StepResult {
    let cluster = world
        .local_cluster
        .as_ref()
        .ok_or(StepError::LogicalError {
            message: "No local cluster available".into(),
        })?;
    let peer_selection = if peers.is_empty() {
        PeerSelection::None
    } else {
        let named = peers
            .iter()
            .map(|peer| world.resolve_node_name(peer))
            .collect::<Result<Vec<String>, StepError>>()?;
        PeerSelection::Named(named)
    };
    let started_node = Box::pin(
        cluster.start_node_with(
            &node_name,
            StartNodeOptions::default()
                .with_peers(peer_selection)
                .with_persist_dir(world.scenario_base_dir.join(node_name.as_str()))
                .create_patch(move |config| {
                    // Placeholder - Add any custom configuration changes here if needed.
                    Ok(config)
                }),
        ),
    )
    .await
    .inspect_err(|e| {
        warn!(
            target: TARGET,
            "Step `I start node/peer node {node_name} (connected to {peers:?})` error: {e}"
        );
    })?;

    let started_node_name = started_node.name.clone();

    world.nodes_info.insert(
        node_name.clone(),
        NodeInfo {
            name: node_name.clone(),
            started_node,
            run_config: None,
            chain_info: HashMap::default(),
        },
    );

    let operation = format!("node '{started_node_name}' readiness");
    track_progress(&operation, Duration::from_secs(5), async {
        cluster
            .wait_node_ready(&started_node_name)
            .await
            .map_err(|source| StepError::StepFail {
                message: format!(
                    "node '{started_node_name}' did not become ready after start: {source}"
                ),
            })
    })
    .await?;

    Ok(())
}

#[when(expr = "node {string} is at height {int} in {int} seconds")]
#[then(expr = "node {string} is at height {int} in {int} seconds")]
async fn node_is_at_height(
    world: &mut CucumberWorld,
    node_name: String,
    height: u64,
    time_out_seconds: u64,
) -> StepResult {
    let start = Instant::now();
    let time_out = Duration::from_secs(time_out_seconds);

    let mut count = 0usize;
    loop {
        poll_all_nodes_and_update_consensus_cache(&mut world.nodes_info).await?;
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
                    "Error: Node '{node_name}' did not reach height {height} in {time_out_seconds} s"
                ),
            });
        }
        sleep(Duration::from_millis(100)).await;
        count += 1;
    }
}

#[when(expr = "all nodes converged to within {int} blocks in {int} seconds")]
#[then(expr = "all nodes converged to within {int} blocks in {int} seconds")]
async fn all_nodes_converged(
    world: &mut CucumberWorld,
    max_diff_height: u64,
    time_out_seconds: u64,
) -> StepResult {
    nodes_converged(world, None, max_diff_height, time_out_seconds).await
}

#[when(
    expr = "all nodes have at least {int} blocks and converged to within {int} blocks in {int} seconds"
)]
#[then(
    expr = "all nodes have at least {int} blocks and converged to within {int} blocks in {int} seconds"
)]
async fn all_nodes_reached_min_height_and_converged(
    world: &mut CucumberWorld,
    min_height: u64,
    max_diff_height: u64,
    time_out_seconds: u64,
) -> StepResult {
    nodes_converged(world, Some(min_height), max_diff_height, time_out_seconds).await
}

#[then(expr = "I stop all nodes")]
fn stop_all_nodes(world: &mut CucumberWorld) -> StepResult {
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

fn tips_aligned_at_min_difference(
    nodes_chain_info: &HashMap<String, ChainInfoMap>,
    all_nodes_min: u64,
) -> (AlignmentStatus, Vec<MaybeSnapshot>) {
    // Always return per-node view at min_height for logging
    let mut anchor_hashes: Vec<MaybeSnapshot> = Vec::with_capacity(nodes_chain_info.len());

    for node_name in nodes_chain_info.keys() {
        let peer_chain = nodes_chain_info
            .get(node_name)
            .expect("nodes_chain_info must be pre-initialized");
        anchor_hashes.push(MaybeSnapshot {
            height: all_nodes_min,
            header_hash: peer_chain.get(&all_nodes_min).cloned(),
        });
    }

    let all_have = anchor_hashes.iter().all(|snap| snap.header_hash.is_some());
    if !all_have {
        return (AlignmentStatus::MissingChainInfo, anchor_hashes);
    }

    let compare_hash = anchor_hashes[0].header_hash.as_ref().unwrap();
    let all_same = anchor_hashes
        .iter()
        .all(|snap| snap.header_hash.as_ref().unwrap() == compare_hash);

    if all_same {
        (AlignmentStatus::Aligned, anchor_hashes)
    } else {
        (AlignmentStatus::Fork, anchor_hashes)
    }
}

async fn fetch_and_update_chain_info(
    nodes_info: &mut HashMap<String, NodeInfo>,
    nodes_chain_info: &mut HashMap<String, ChainInfoMap>,
) -> Result<(u64, u64, Vec<u64>), StepError> {
    poll_all_nodes_and_update_consensus_cache(nodes_info).await?;

    let mut best_node_heights: Vec<u64> = Vec::with_capacity(nodes_info.len());

    for node_info in nodes_info.values() {
        let max_height = node_info.best_height().unwrap_or_default();
        best_node_heights.push(max_height);

        let started_node_name = node_info.started_node.name.clone();
        let chain =
            nodes_chain_info
                .get_mut(&started_node_name)
                .ok_or(StepError::LogicalError {
                    message: format!(
                        "Started node '{}' not found in chain info map",
                        &started_node_name
                    ),
                })?;
        let chain_info = node_info.chain_info();
        for (height, hash) in chain_info {
            chain.insert(*height, hash.clone());
        }
    }

    let all_nodes_min = *best_node_heights.iter().min().unwrap_or(&0);
    let all_nodes_max = *best_node_heights.iter().max().unwrap_or(&0);
    let diff = all_nodes_max - all_nodes_min;

    Ok((all_nodes_min, diff, best_node_heights))
}

fn log_waiting_status(
    status: &AlignmentStatus,
    min_height: Option<u64>,
    diff: u64,
    peer_heights: &[u64],
    peer_min: u64,
    anchor_hashes: &[MaybeSnapshot],
    start: Instant,
) {
    match status {
        AlignmentStatus::Aligned => {
            let converge = min_height.map_or_else(
                || "Waiting for all nodes to converge".to_owned(),
                |min_height| format!("Waiting for at least {min_height} blocks converged"),
            );
            info!(
                target: TARGET,
                "{converge} - elapsed: {:.2?}, diff: {diff}, heights: {peer_heights:?}",
                start.elapsed()
            );
        }
        AlignmentStatus::MissingChainInfo => {
            info!(
                target: TARGET,
                "Waiting for all node's hashes at height {peer_min} - elapsed: {:.2?}, diff: \
                {diff}, heights: {peer_heights:?}, anchors: {:?}",
                start.elapsed(),
                anchor_hashes.iter().map(|snap| &snap.header_hash).collect::<Vec<_>>()
            );
        }
        AlignmentStatus::Fork => {
            let fork_hashes: std::collections::HashSet<_> = anchor_hashes
                .iter()
                .filter_map(|snap| snap.header_hash.as_ref())
                .collect();
            info!(
                target: TARGET,
                "{} fork chains detected!!! Elapsed: {:.2?}, diff: {diff}, heights: {peer_heights:?}, \
                fork hashes at height {}: {:?}",
                fork_hashes.len(), start.elapsed(), anchor_hashes[0].height, fork_hashes
            );
        }
    }
}

async fn nodes_converged(
    world: &mut CucumberWorld,
    min_height: Option<u64>,
    max_diff_height: u64,
    time_out_seconds: u64,
) -> StepResult {
    let nodes_info = &world.nodes_info.values().collect::<Vec<&NodeInfo>>();
    let start = Instant::now();
    let time_out = Duration::from_secs(time_out_seconds);

    // node_name -> (height -> header_id)  (overwrites on reorg)
    let mut nodes_chain_info: HashMap<String, ChainInfoMap> =
        HashMap::with_capacity(nodes_info.len());

    // Pre-initialize so lookups are deterministic
    for node_info in nodes_info {
        nodes_chain_info
            .entry(node_info.started_node.name.clone())
            .or_default();
    }

    let mut count = 0usize;
    loop {
        let (all_nodes_min, diff, peer_heights) =
            fetch_and_update_chain_info(&mut world.nodes_info, &mut nodes_chain_info).await?;
        let (status, anchor_hashes) =
            tips_aligned_at_min_difference(&nodes_chain_info, all_nodes_min);

        if diff <= max_diff_height
            && matches!(status, AlignmentStatus::Aligned)
            && all_nodes_min >= min_height.unwrap_or_default()
        {
            if let Some(min_height) = min_height {
                info!(
                    target: TARGET,
                    "All nodes have at least {min_height} blocks, converged in {:.2?} - max diff: \
                    {diff}, heights: {peer_heights:?}",
                    start.elapsed()
                );
            } else {
                info!(
                    target: TARGET,
                    "All nodes converged in {:.2?} - max diff: {diff}, heights: {peer_heights:?}",
                    start.elapsed()
                );
            }
            return Ok(());
        }

        if count.is_multiple_of(50) {
            log_waiting_status(
                &status,
                min_height,
                diff,
                &peer_heights,
                all_nodes_min,
                &anchor_hashes,
                start,
            );
        }

        if start.elapsed() >= time_out {
            let err = min_height.map_or_else(|| StepError::StepFail {
                message: format!(
                    "Error: Nodes did not converge to {max_diff_height} blocks at in \
                    {time_out_seconds} s"
                ),
            }, |min_height| StepError::StepFail {
                message: format!(
                    "Error: Nodes did not converge to {max_diff_height} blocks at minimum height \
                    {min_height} in {time_out_seconds} s"
                ),
            });
            return Err(err);
        }

        sleep(Duration::from_millis(100)).await;
        count += 1;
    }
}

async fn poll_all_nodes_and_update_consensus_cache(
    nodes_info: &mut HashMap<String, NodeInfo>,
) -> Result<(), StepError> {
    let nodes = nodes_info.values().collect::<Vec<&NodeInfo>>();
    let info_futures = nodes.iter().map(async |node| {
        let node_name = node.name.clone();
        node.started_node
            .client
            .consensus_info()
            .await
            .map(|info| ConsensusSnapshot {
                node_name,
                height: info.height,
                header_hash: info.tip.encode_hex(),
            })
    });

    let snapshots: Vec<ConsensusSnapshot> = try_join_all(info_futures).await.inspect_err(|e| {
        warn!(
            target: TARGET,
            "Error: Some node(s) did not respond with their consensus_info: {e}",
        );
    })?;
    for snap in &snapshots {
        let node = nodes_info
            .get_mut(&snap.node_name)
            .ok_or(StepError::LogicalError {
                message: format!(
                    "Runtime node '{}' not found in world.nodes_info",
                    snap.node_name
                ),
            })?;
        node.upsert_tip(snap.height, snap.header_hash.clone());
    }

    Ok(())
}
