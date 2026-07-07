use std::{collections::HashMap, time::Duration};

use hex::ToHex as _;
use lb_chain_service::CryptarchiaInfo;
use lb_testing_framework::{BlockFeed, NodeHttpClient, is_truthy_env};
use tokio::{
    task::JoinSet,
    time::{Instant, sleep, timeout},
};
use tracing::{info, warn};

use crate::{
    common::wallet::WalletChainSourceId,
    cucumber::{
        defaults::CUCUMBER_VERBOSE_CONSOLE, error::StepError, wallet::TARGET, world::CucumberWorld,
    },
};

const BEST_NODE_SELECTION_TIMEOUT: Duration = Duration::from_mins(3);
const BEST_NODE_SELECTION_POLL_INTERVAL: Duration = Duration::from_millis(200);
const BEST_NODE_SELECTION_LOG_INTERVAL: Duration = Duration::from_secs(5);
const BEST_NODE_QUERY_TIMEOUT: Duration = Duration::from_secs(2);

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
    /// All nodes in the selected group that shared this winning tip at
    /// resolution time. Includes `node_name`, sorted lexicographically, and
    /// deduplicated for stable fanout.
    pub same_tip_nodes: Vec<String>,
}

impl BestNodeInfo {
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

    /// Return the best node for the group in which `wallet_name` resides.
    pub fn best_node_for_wallet<'a>(
        &'a self,
        world: &'a CucumberWorld,
        wallet_name: &str,
    ) -> Result<String, StepError> {
        let wallet_node_name = world.resolve_wallet_node_name(wallet_name)?;
        let Some(node) = self.for_wallet_node(&wallet_node_name, &world.node_to_group) else {
            return Err(StepError::LogicalError {
                message: format!("Best node for '{wallet_name}' not found in world state"),
            });
        };
        let Some(node_info) = world.nodes_info.get(&node.node_name) else {
            return Err(StepError::LogicalError {
                message: format!("Best node '{}' not found in world state", node.node_name),
            });
        };
        Ok(node_info.name.clone())
    }
}

/// Determine the best node to use for all block queries, with an optional hint
/// from a previous selection.
pub async fn sanitize_best_node_info<'a>(
    world: &'a CucumberWorld,
    wallet_name: &str,
    best_node_info: Option<&BestNodeInfo>,
) -> Result<(String, &'a NodeHttpClient, CryptarchiaInfo), StepError> {
    sanitize_best_node_info_with_feed(world, wallet_name, best_node_info, None).await
}

/// Resolve a query node for `wallet_name`, reusing `best_node_info` when it is
/// still on the observed chain.
///
/// If the cached selection is stale or missing, this waits for a fresh majority
/// tip in the wallet's fork group and returns the selected node client.
pub async fn sanitize_best_node_info_with_feed<'a>(
    world: &'a CucumberWorld,
    wallet_name: &str,
    best_node_info: Option<&BestNodeInfo>,
    feed: Option<&BlockFeed>,
) -> Result<(String, &'a NodeHttpClient, CryptarchiaInfo), StepError> {
    let wallet_node_name = world.resolve_wallet_node_name(wallet_name)?;

    let mut last_msg = String::new();
    if let Some(best_info) = best_node_info
        && let Some(node) = best_info.for_wallet_node(&wallet_node_name, &world.node_to_group)
    {
        if let Some(selection) = resolve_cached_best_node(world, node, feed).await? {
            return Ok(selection);
        }

        let refreshed = determine_best_node(world, &wallet_node_name, Some(&mut last_msg)).await?;
        return resolve_selected_best_node(world, &wallet_node_name, &refreshed).await;
    }

    let refreshed = determine_best_node(world, &wallet_node_name, Some(&mut last_msg)).await?;
    resolve_selected_best_node(world, &wallet_node_name, &refreshed).await
}

/// Resolve a query node for a wallet-feed source group.
///
/// This is the group-oriented form used when the caller already knows the
/// source id instead of a wallet name.
pub async fn sanitize_best_node_info_for_group<'a>(
    world: &'a CucumberWorld,
    source_id: &WalletChainSourceId,
    best_node_info: Option<&BestNodeInfo>,
) -> Result<(String, &'a NodeHttpClient, CryptarchiaInfo), StepError> {
    sanitize_best_node_info_for_group_with_feed(world, source_id, best_node_info, None).await
}

/// Resolve a query node for a source group, optionally validating against a
/// block feed.
///
/// The feed check keeps a cached best node only if its tip is still part of the
/// observed chain.
pub async fn sanitize_best_node_info_for_group_with_feed<'a>(
    world: &'a CucumberWorld,
    source_id: &WalletChainSourceId,
    best_node_info: Option<&BestNodeInfo>,
    feed: Option<&BlockFeed>,
) -> Result<(String, &'a NodeHttpClient, CryptarchiaInfo), StepError> {
    let group_key = source_id.as_str();
    let representative_node = representative_node_for_group(world, group_key)?;

    let mut last_msg = String::new();
    if let Some(best_info) = best_node_info
        && let Some(node) = best_info
            .best_nodes
            .get(group_key)
            .or_else(|| best_info.best_nodes.get(""))
    {
        if let Some(selection) = resolve_cached_best_node(world, node, feed).await? {
            return Ok(selection);
        }

        let refreshed =
            determine_best_node(world, &representative_node, Some(&mut last_msg)).await?;
        return resolve_selected_best_node(world, &representative_node, &refreshed).await;
    }

    let refreshed = determine_best_node(world, &representative_node, Some(&mut last_msg)).await?;
    resolve_selected_best_node(world, &representative_node, &refreshed).await
}

/// Determine the best node to query, scoped to the fork group that contains
/// `wallet_node_name`.
///
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
    last_verbose_msg: Option<&mut String>,
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
        let (mut ordered_snapshots, mut unreachable) =
            collect_ordered_group_snapshots(world, &candidates).await;
        unreachable.sort();
        if !unreachable.is_empty() {
            warn!(
                target: TARGET,
                "Best-node selection unreachable nodes in group '{}': {}",
                display_group_key(&group_key),
                unreachable.join(", ")
            );
        }

        let responsive_count = ordered_snapshots.len();

        if responsive_count > 0 {
            last_group_summary = summarize_tip_groups(&ordered_snapshots);

            if let Some(majority_group) = select_majority_tip_group(&ordered_snapshots)
                && let Some(best_idx) =
                    select_best_snapshot_index(&ordered_snapshots, &majority_group)
            {
                let same_tip_nodes = stable_unique_node_names(
                    majority_group
                        .iter()
                        .map(|idx| ordered_snapshots[*idx].node_name.clone()),
                );
                let best_snapshot = ordered_snapshots.swap_remove(best_idx);
                let best_node_name = best_snapshot.node_name;
                let best_consensus = best_snapshot.consensus;
                let majority_size = majority_group.len();

                if is_truthy_env(CUCUMBER_VERBOSE_CONSOLE) {
                    let this_msg = format!(
                        "Chosen best node {best_node_name} in group '{}' with block height: '{}' \
                        header id: '{}' (majority {}/{})",
                        display_group_key(&group_key),
                        best_consensus.height,
                        best_consensus.tip,
                        majority_size,
                        responsive_count
                    );
                    if let Some(last) = last_verbose_msg {
                        if last != &this_msg {
                            last.clone_from(&this_msg);
                            info!(target: TARGET, "{this_msg}");
                        }
                    } else {
                        info!(target: TARGET, "{this_msg}");
                    }
                }

                return Ok(BestNodeInfo {
                    best_nodes: HashMap::from([(
                        group_key.clone(),
                        BestGroupNode {
                            node_name: best_node_name,
                            tip: best_consensus.tip.encode_hex::<String>(),
                            height: best_consensus.height,
                            same_tip_nodes,
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

/// Get best-node info for the wallet's fork group.
/// Wait for and return the best-node selection for `wallet_name`.
///
/// The selected node is chosen from the wallet's fork group. The optional log
/// string suppresses repeated verbose messages while polling.
pub async fn get_best_node_info(
    world: &CucumberWorld,
    wallet_name: &str,
    last_verbose_msg: Option<&mut String>,
) -> Result<BestNodeInfo, StepError> {
    let wallet = world.resolve_wallet(wallet_name)?;
    determine_best_node(world, &wallet.node_name, last_verbose_msg).await
}

#[derive(Debug)]
struct NodeConsensusSnapshot {
    node_name: String,
    consensus: CryptarchiaInfo,
}

fn representative_node_for_group(
    world: &CucumberWorld,
    group_key: &str,
) -> Result<String, StepError> {
    if world.node_groups.is_empty() {
        let mut candidates = world.all_node_names();
        candidates.sort();
        return candidates
            .into_iter()
            .next()
            .ok_or_else(|| StepError::LogicalError {
                message: "No available nodes to query for UTXOs".to_owned(),
            });
    }

    let mut candidates = world
        .node_groups
        .get(group_key)
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Node group '{group_key}' was not found in scenario state"),
        })?
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    candidates.sort();
    candidates
        .into_iter()
        .next()
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Node group '{group_key}' has no nodes"),
        })
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

async fn resolve_cached_best_node<'a>(
    world: &'a CucumberWorld,
    selected: &BestGroupNode,
    feed: Option<&BlockFeed>,
) -> Result<Option<(String, &'a NodeHttpClient, CryptarchiaInfo)>, StepError> {
    let Some(node_info) = world.nodes_info.get(&selected.node_name) else {
        return Err(StepError::LogicalError {
            message: format!(
                "Best node '{}' not found in world state",
                selected.node_name
            ),
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

    let selected_tip = normalize_header_id_str(&selected.tip);
    let live_tip = consensus.cryptarchia_info.tip.encode_hex::<String>();
    let tip_or_height_changed =
        selected_tip != live_tip || consensus.cryptarchia_info.height != selected.height;

    if tip_or_height_changed && !selected_tip_still_on_observed_chain(feed, selected) {
        return Ok(None);
    }

    Ok(Some((
        selected.node_name.clone(),
        &node_info.started_node.client,
        consensus.cryptarchia_info,
    )))
}

fn selected_tip_still_on_observed_chain(
    feed: Option<&BlockFeed>,
    selected: &BestGroupNode,
) -> bool {
    let Some(observation) = feed.and_then(BlockFeed::latest_observation) else {
        return false;
    };
    let Some(node_chain) = observation.node(&selected.node_name) else {
        return false;
    };
    let Some(observed_header) = node_chain.header_at_height(selected.height) else {
        return false;
    };

    normalize_header_id_str(&selected.tip) == observed_header.encode_hex::<String>()
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

/// Query all candidate nodes in parallel and return their consensus snapshots,
/// sorted by node name for stable ordering. Nodes that do not respond within 2
/// seconds are excluded from the result, and their names are returned in the
/// `unreachable` vector.
async fn collect_ordered_group_snapshots(
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
    snapshots.sort_by(|a, b| a.node_name.cmp(&b.node_name));

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

/// Returns the index of the best snapshot in the majority group, or None if the
/// group is empty. If `ordered_snapshots` did not change between calls, it will
/// always return the same best node index.
fn select_best_snapshot_index(
    ordered_snapshots: &[NodeConsensusSnapshot],
    majority_group: &[usize],
) -> Option<usize> {
    let best_height = majority_group
        .iter()
        .map(|idx| ordered_snapshots[*idx].consensus.height)
        .max()?;

    let best_candidates = majority_group
        .iter()
        .copied()
        .filter(|idx| ordered_snapshots[*idx].consensus.height == best_height)
        .collect::<Vec<_>>();

    best_candidates.first().copied()
}

fn normalize_header_id_str(header_id: &str) -> String {
    header_id
        .trim()
        .trim_start_matches("0x")
        .to_ascii_lowercase()
}

fn stable_unique_node_names(node_names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut node_names = node_names.into_iter().collect::<Vec<_>>();
    node_names.sort();
    node_names.dedup();
    node_names
}

#[cfg(test)]
mod tests {
    use super::stable_unique_node_names;

    #[test]
    fn stable_unique_node_names_sorts_and_deduplicates() {
        let node_names = vec![
            "NODE_3".to_owned(),
            "NODE_1".to_owned(),
            "NODE_2".to_owned(),
            "NODE_1".to_owned(),
        ];

        assert_eq!(
            stable_unique_node_names(node_names),
            vec![
                "NODE_1".to_owned(),
                "NODE_2".to_owned(),
                "NODE_3".to_owned(),
            ]
        );
    }
}
