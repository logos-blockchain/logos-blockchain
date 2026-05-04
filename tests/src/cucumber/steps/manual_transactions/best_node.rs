use std::{collections::HashMap, time::Duration};

use hex::ToHex as _;
use lb_chain_service::CryptarchiaInfo;
use lb_testing_framework::{NodeHttpClient, is_truthy_env};
use tokio::{
    task::JoinSet,
    time::{Instant, sleep, timeout},
};
use tracing::{info, warn};

use crate::cucumber::{
    defaults::CUCUMBER_VERBOSE_CONSOLE, error::StepError, steps::TARGET, world::CucumberWorld,
};

/// Best-node selection result, keyed by group name.
/// When no groups are configured the single key is the empty string "".
#[derive(Clone, Debug)]
pub struct BestNodeInfo {
    /// `group_name` -> best node in that group.
    pub best_nodes: HashMap<String, BestGroupNode>,
}

/// The winning node for one fork group.
#[derive(Clone, Debug)]
pub struct BestGroupNode {
    /// Logical node name (e.g. `NODE_1`).
    pub node_name: String,
    /// Chain tip header id at selection time (hex string, "0x...").
    pub tip: String,
    /// Chain height at selection time.
    pub height: u64,
}

impl BestNodeInfo {
    /// Return the best node for the group that owns `node_name`,
    /// or the single ungrouped best node when no groups are defined.
    #[must_use]
    pub fn for_node(&self, node_name: &str) -> Option<&BestGroupNode> {
        self.best_nodes
            .get(node_name)
            .or_else(|| self.best_nodes.get(""))
    }

    /// Return the best node for the group that owns `wallet_node`,
    /// given the reverse-lookup map.
    #[must_use]
    pub fn for_wallet_node<'a>(
        &'a self,
        wallet_node: &str,
        node_to_group: &HashMap<String, String>,
    ) -> Option<&'a BestGroupNode> {
        node_to_group
            .get(wallet_node)
            .and_then(|group| self.best_nodes.get(group.as_str()))
            .or_else(|| self.best_nodes.get(""))
    }
}

const BEST_NODE_SELECTION_TIMEOUT: Duration = Duration::from_mins(3);
const BEST_NODE_SELECTION_POLL_INTERVAL: Duration = Duration::from_millis(200);
const BEST_NODE_SELECTION_LOG_INTERVAL: Duration = Duration::from_secs(5);
const BEST_NODE_QUERY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug)]
struct NodeConsensusSnapshot {
    node_name: String,
    consensus: CryptarchiaInfo,
}

/// Determine the best node to use for all block queries, with an optional hint
/// from a previous selection.
pub async fn sanitize_best_node_info<'a>(
    world: &'a CucumberWorld,
    wallet_name: &str,
    best_node_info: Option<&'a BestNodeInfo>,
) -> Result<(String, &'a NodeHttpClient, CryptarchiaInfo), StepError> {
    let wallet_node_name = world.resolve_wallet_node_name(wallet_name)?;

    if let Some(best_info) = best_node_info
        && let Some(node) = best_info.for_wallet_node(&wallet_node_name, &world.node_to_group)
    {
        let Some(node_info) = world.nodes_info.get(&node.node_name) else {
            return Err(StepError::LogicalError {
                message: format!("Best node '{}' not found in world state", node.node_name),
            });
        };

        let consensus = node_info
            .started_node
            .client
            .consensus_info()
            .await
            .map_err(|_| StepError::LogicalError {
                message: "No available nodes to query for UTXOs".to_owned(),
            })?;

        let selected_tip = normalize_header_id_str(&node.tip);
        let live_tip = consensus.cryptarchia_info.tip.encode_hex::<String>();
        let tip_or_height_changed =
            selected_tip != live_tip || consensus.cryptarchia_info.height != node.height;

        if tip_or_height_changed
            && !is_tip_still_on_canonical_chain(
                &node_info.started_node.client,
                &node.tip,
                node.height,
                &consensus.cryptarchia_info,
            )
            .await?
        {
            let refreshed = determine_best_node(world, &wallet_node_name).await?;
            return resolve_selected_best_node(world, &wallet_node_name, &refreshed).await;
        }

        return Ok((
            node.node_name.clone(),
            &node_info.started_node.client,
            consensus.cryptarchia_info,
        ));
    }

    let refreshed = determine_best_node(world, &wallet_node_name).await?;
    resolve_selected_best_node(world, &wallet_node_name, &refreshed).await
}

async fn resolve_selected_best_node<'a>(
    world: &'a CucumberWorld,
    wallet_node_name: &str,
    best_info: &BestNodeInfo,
) -> Result<(String, &'a NodeHttpClient, CryptarchiaInfo), StepError> {
    let selected = best_info
        .for_wallet_node(wallet_node_name, &world.node_to_group)
        .ok_or(StepError::LogicalError {
            message: format!("No best-node entry found for wallet node '{wallet_node_name}'"),
        })?;

    let node_info = world
        .nodes_info
        .get(&selected.node_name)
        .ok_or(StepError::LogicalError {
            message: format!(
                "Best node '{}' not found in world state",
                selected.node_name
            ),
        })?;

    let consensus = node_info
        .started_node
        .client
        .consensus_info()
        .await
        .map_err(|_| StepError::LogicalError {
            message: "No available nodes to query for UTXOs".to_owned(),
        })?;

    Ok((
        selected.node_name.clone(),
        &node_info.started_node.client,
        consensus.cryptarchia_info,
    ))
}

/// Determine the best node to query, scoped to the fork group that contains
/// `wallet_node_name`.
/// Falls back to all nodes when no groups are configured.
/// Nodes that do not respond within 2 seconds are excluded from the majority
/// denominator.
#[expect(
    clippy::cognitive_complexity,
    reason = "Selection loop includes polling, timeout handling, and majority/tie logic."
)]
pub async fn determine_best_node(
    world: &CucumberWorld,
    wallet_node_name: &str,
) -> Result<BestNodeInfo, StepError> {
    let (group_key, candidates) = resolve_candidate_nodes(world, wallet_node_name)?;
    if candidates.is_empty() {
        return Err(StepError::LogicalError {
            message: "No available nodes to query for UTXOs".to_owned(),
        });
    }

    let start = Instant::now();
    let mut last_log_at: Option<Instant> = None;
    let mut last_group_summary = String::from("no responsive nodes");

    loop {
        let (mut snapshots, mut unreachable) = collect_group_snapshots(world, &candidates).await;
        unreachable.sort();
        if !unreachable.is_empty() {
            warn!(
                target: TARGET,
                "Best-node selection unreachable nodes in group '{}': {}",
                display_group_key(&group_key),
                unreachable.join(", ")
            );
        }

        let responsive_count = snapshots.len();

        if responsive_count > 0 {
            last_group_summary = summarize_tip_groups(&snapshots);

            if let Some(majority_group) = select_majority_tip_group(&snapshots)
                && let Some(best_idx) = select_best_snapshot_index(&snapshots, &majority_group)
            {
                let best_snapshot = snapshots.swap_remove(best_idx);
                let best_node_name = best_snapshot.node_name;
                let best_consensus = best_snapshot.consensus;
                let majority_size = majority_group.len();

                if is_truthy_env(CUCUMBER_VERBOSE_CONSOLE) {
                    info!(
                        target: TARGET,
                        "Chosen best node {best_node_name} in group '{}' with block height: '{}' \
                        header id: '{}' (majority {}/{})",
                        display_group_key(&group_key),
                        best_consensus.height,
                        best_consensus.tip,
                        majority_size,
                        responsive_count
                    );
                }

                return Ok(BestNodeInfo {
                    best_nodes: HashMap::from([(
                        group_key.clone(),
                        BestGroupNode {
                            node_name: best_node_name,
                            tip: best_consensus.tip.encode_hex::<String>(),
                            height: best_consensus.height,
                        },
                    )]),
                });
            }
        }

        if start.elapsed() >= BEST_NODE_SELECTION_TIMEOUT {
            return Err(StepError::LogicalError {
                message: format!(
                    "No stable majority tip across candidate nodes for group '{}' after {:.2?}. \
                    Reachable nodes: {}/{}. Tip groups: {}",
                    display_group_key(&group_key),
                    start.elapsed(),
                    responsive_count,
                    candidates.len(),
                    last_group_summary
                ),
            });
        }

        if last_log_at.is_none_or(|last| last.elapsed() >= BEST_NODE_SELECTION_LOG_INTERVAL) {
            info!(
                target: TARGET,
                "Waiting for consensus majority tip before selecting best node for group '{}' - \
                elapsed: {:.2?}, reachable: {}/{}, tips: {}",
                display_group_key(&group_key),
                start.elapsed(),
                responsive_count,
                candidates.len(),
                last_group_summary
            );
            last_log_at = Some(Instant::now());
        }

        sleep(BEST_NODE_SELECTION_POLL_INTERVAL).await;
    }
}

const fn display_group_key(group_key: &str) -> &str {
    if group_key.is_empty() {
        "<ungrouped>"
    } else {
        group_key
    }
}

fn resolve_candidate_nodes(
    world: &CucumberWorld,
    wallet_node_name: &str,
) -> Result<(String, Vec<String>), StepError> {
    if world.node_groups.is_empty() {
        let mut candidates = world.all_node_names();
        candidates.sort();
        return Ok((String::new(), candidates));
    }

    let group_name = world
        .node_to_group
        .get(wallet_node_name)
        .ok_or(StepError::LogicalError {
            message: format!("Node '{wallet_node_name}' is not in any configured node group"),
        })?;

    let mut candidates = world
        .node_groups
        .get(group_name)
        .ok_or(StepError::LogicalError {
            message: format!("Node group '{group_name}' was not found in scenario state"),
        })?
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    candidates.sort();

    Ok((group_name.clone(), candidates))
}

async fn collect_group_snapshots(
    world: &CucumberWorld,
    candidates: &[String],
) -> (Vec<NodeConsensusSnapshot>, Vec<String>) {
    let mut snapshots = Vec::with_capacity(candidates.len());
    let mut unreachable = Vec::new();
    let mut jobs = JoinSet::new();

    for node_name in candidates {
        let Some(node) = world.nodes_info.get(node_name) else {
            unreachable.push(node_name.clone());
            continue;
        };

        let node_name = node_name.clone();
        let client = node.started_node.client.clone();
        jobs.spawn(async move {
            match timeout(BEST_NODE_QUERY_TIMEOUT, client.consensus_info()).await {
                Ok(Ok(consensus)) => Some(NodeConsensusSnapshot {
                    node_name,
                    consensus: consensus.cryptarchia_info,
                }),
                Ok(Err(_)) | Err(_) => None,
            }
        });
    }

    while let Some(result) = jobs.join_next().await {
        if let Ok(Some(snapshot)) = result {
            snapshots.push(snapshot);
        }
    }

    let responsive_names = snapshots
        .iter()
        .map(|snapshot| snapshot.node_name.as_str())
        .collect::<std::collections::HashSet<_>>();
    for candidate in candidates {
        if !responsive_names.contains(candidate.as_str()) && !unreachable.contains(candidate) {
            unreachable.push(candidate.clone());
        }
    }

    (snapshots, unreachable)
}

fn tip_key(consensus: &CryptarchiaInfo) -> String {
    consensus.tip.encode_hex::<String>()
}

fn summarize_tip_groups(snapshots: &[NodeConsensusSnapshot]) -> String {
    // tip -> (node_count, max_height)
    let mut grouped: HashMap<String, (usize, u64)> = HashMap::new();
    for snapshot in snapshots {
        let entry = grouped
            .entry(tip_key(&snapshot.consensus))
            .or_insert((0, 0));
        entry.0 += 1;
        entry.1 = entry.1.max(snapshot.consensus.height);
    }

    let mut groups = grouped.into_iter().collect::<Vec<_>>();
    groups.sort_by(
        |(left_tip, (left_count, left_max_height)),
         (right_tip, (right_count, right_max_height))| {
            right_count
                .cmp(left_count)
                .then_with(|| right_max_height.cmp(left_max_height))
                .then_with(|| right_tip.cmp(left_tip))
        },
    );

    groups
        .into_iter()
        .map(|(tip, (count, max_height))| {
            let tip_prefix: String = tip.chars().take(16).collect();
            format!("{tip_prefix}..({count},h={max_height})")
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

fn select_majority_tip_group(snapshots: &[NodeConsensusSnapshot]) -> Option<Vec<usize>> {
    let mut grouped: HashMap<String, Vec<usize>> = HashMap::new();
    let responsive_nodes = snapshots.len();
    for (idx, snapshot) in snapshots.iter().enumerate() {
        grouped
            .entry(tip_key(&snapshot.consensus))
            .or_default()
            .push(idx);
    }

    grouped
        .into_values()
        .filter(|idxs| idxs.len() * 2 > responsive_nodes)
        .max_by(|left, right| {
            let left_best_height = left
                .iter()
                .map(|idx| snapshots[*idx].consensus.height)
                .max()
                .unwrap_or_default();
            let right_best_height = right
                .iter()
                .map(|idx| snapshots[*idx].consensus.height)
                .max()
                .unwrap_or_default();

            left.len()
                .cmp(&right.len())
                .then_with(|| left_best_height.cmp(&right_best_height))
        })
}

fn select_best_snapshot_index(
    snapshots: &[NodeConsensusSnapshot],
    majority_group: &[usize],
) -> Option<usize> {
    majority_group
        .iter()
        .copied()
        .max_by(|left_idx, right_idx| {
            let left = &snapshots[*left_idx];
            let right = &snapshots[*right_idx];

            left.consensus
                .height
                .cmp(&right.consensus.height)
                .then_with(|| right.node_name.cmp(&left.node_name))
        })
}

/// Get best-node info for the wallet's fork group.
pub async fn get_best_node_info(
    world: &CucumberWorld,
    wallet_name: &str,
) -> Result<BestNodeInfo, StepError> {
    let wallet = world.resolve_wallet(wallet_name)?;
    determine_best_node(world, &wallet.node_name).await
}

fn normalize_header_id_str(header_id: &str) -> String {
    header_id
        .trim()
        .trim_start_matches("0x")
        .to_ascii_lowercase()
}

async fn is_tip_still_on_canonical_chain(
    client: &NodeHttpClient,
    selected_tip: &str,
    selected_height: u64,
    live_consensus: &CryptarchiaInfo,
) -> Result<bool, StepError> {
    let selected_tip_normalized = normalize_header_id_str(selected_tip);
    let live_tip_normalized = live_consensus.tip.encode_hex::<String>();

    if selected_tip_normalized == live_tip_normalized {
        return Ok(true);
    }

    if live_consensus.height < selected_height {
        return Ok(false);
    }

    let mut current_header_id = live_consensus.tip;
    let mut remaining_steps = live_consensus.height - selected_height;

    while remaining_steps > 0 {
        let Some(block) = client.block(&current_header_id).await? else {
            return Ok(false);
        };
        current_header_id = block.header.parent_block;
        remaining_steps -= 1;
    }

    Ok(normalize_header_id_str(&current_header_id.to_string()) == selected_tip_normalized)
}
