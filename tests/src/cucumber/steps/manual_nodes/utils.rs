use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use cucumber::{
    gherkin::{Step, Table},
    given, when,
};
use futures_util::future::try_join_all;
use hex::ToHex as _;
use lb_core::mantle::{GenesisTx as _, Transaction as _, Utxo};
use lb_node::config::RunConfig;
use lb_testing_framework::{LbcManualCluster, NodeHttpClient, USER_CONFIG_FILE};
use testing_framework_core::scenario::{PeerSelection, StartNodeOptions};
use tokio::time::{Instant, sleep};
use tracing::{info, warn};

use crate::cucumber::{
    error::{StepError, StepResult},
    steps::TARGET,
    utils::{extract_child_dir_name, peer_id_from_node_yaml, track_progress},
    world::{ChainInfoMap, CucumberWorld, GenesisTokens, NodeInfo, WalletInfo, WalletInfoMap},
};

type NodesToStartUnordered = HashMap<String, (Vec<WalletStartInfo>, Vec<String>)>;
type NodesToStartOrdered = Vec<(String, Vec<WalletStartInfo>, Vec<String>)>;

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

#[must_use]
pub fn genesis_block_utxos(genesis_tx: &lb_core::mantle::genesis_tx::GenesisTx) -> Vec<Utxo> {
    let ledger_tx = genesis_tx.mantle_tx().ledger_tx.clone();
    let tx_hash = ledger_tx.hash();

    ledger_tx
        .outputs
        .iter()
        .enumerate()
        .map(|(idx, note)| Utxo::new(tx_hash, idx, *note))
        .collect()
}

const ACCOUNT_INDEX: &str = "account_index";
const ACCOUNT_INDEX_IDX_T1: usize = 0;
const TOKEN_COUNT: &str = "token_count";
const TOKEN_COUNT_IDX: usize = 1;
const TOKEN_AMOUNT: &str = "token_amount";
const TOKEN_AMOUNT_IDX: usize = 2;

fn verify_genesis_wallet_resources_table_indexes(
    table: &Table,
    step: &str,
) -> Result<(), StepError> {
    if table.rows.is_empty()
        || table.rows[0].len() != 3
        || table.rows[0][ACCOUNT_INDEX_IDX_T1] != ACCOUNT_INDEX
        || table.rows[0][TOKEN_COUNT_IDX] != TOKEN_COUNT
        || table.rows[0][TOKEN_AMOUNT_IDX] != TOKEN_AMOUNT
    {
        return Err(StepError::InvalidArgument {
            message: format!(
                "Step `{step}` error: Wallet resources table must have a header row with columns: \
                {ACCOUNT_INDEX}, {TOKEN_COUNT}, {TOKEN_AMOUNT}"
            ),
        });
    }
    // All wallet account indexes must be unique
    let wallet_accounts: HashSet<_> = table
        .rows
        .iter()
        .map(|row| &row[ACCOUNT_INDEX_IDX_T1])
        .collect();
    if wallet_accounts.len() != table.rows.len() {
        return Err(StepError::InvalidArgument {
            message: format!(
                "Step `{step}` error: Duplicate {ACCOUNT_INDEX} indexes found in the table"
            ),
        });
    }

    Ok(())
}

fn parse_genesis_wallet_tokens_row(
    step: &str,
    row: &[String],
) -> Result<(usize, usize, u64), StepError> {
    let account_index =
        row[ACCOUNT_INDEX_IDX_T1]
            .parse::<usize>()
            .map_err(|_| StepError::InvalidArgument {
                message: format!("Step `{step}` error: {ACCOUNT_INDEX} must be a valid number"),
            })?;
    let token_count =
        row[TOKEN_COUNT_IDX]
            .parse::<usize>()
            .map_err(|_| StepError::InvalidArgument {
                message: format!("Step `{step}` error: {TOKEN_COUNT} must be a valid number"),
            })?;
    let token_amount =
        row[TOKEN_AMOUNT_IDX]
            .parse::<u64>()
            .map_err(|_| StepError::InvalidArgument {
                message: format!("Step `{step}` error: {TOKEN_AMOUNT} must be a valid number"),
            })?;
    Ok((account_index, token_count, token_amount))
}

#[given("the genesis block has the following wallet resources:")]
fn cluster_has_wallet_resources(world: &mut CucumberWorld, step: &Step) -> StepResult {
    let table = step
        .table
        .as_ref()
        .ok_or(StepError::MissingTable)
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step.value);
        })?;

    verify_genesis_wallet_resources_table_indexes(table, &step.value)?;
    world.genesis_tokens.clear();
    for row in table.rows.iter().skip(1) {
        let (account_index, token_count, token_amount) =
            parse_genesis_wallet_tokens_row(&step.value, row)?;

        world.genesis_tokens.push(GenesisTokens {
            account_index,
            token_count,
            token_amount,
        });
    }

    Ok(())
}

const NODE_NAME: &str = "node_name";
const NODE_NAME_IDX: usize = 0;
const ACCOUNT_INDEX_IDX_T2: usize = 1;
const WALLET_NAME: &str = "wallet_name";
const WALLET_NAME_IDX: usize = 2;
const CONNECTED_TO: &str = "connected_to";
const CONNECTED_TO_IDX: usize = 3;

fn verify_node_wallet_resources_table_indexes(table: &Table, step: &str) -> Result<(), StepError> {
    if table.rows.is_empty()
        || table.rows[0].len() != 4
        || table.rows[0][NODE_NAME_IDX] != NODE_NAME
        || table.rows[0][ACCOUNT_INDEX_IDX_T2] != ACCOUNT_INDEX
        || table.rows[0][WALLET_NAME_IDX] != WALLET_NAME
        || table.rows[0][CONNECTED_TO_IDX] != CONNECTED_TO
    {
        return Err(StepError::InvalidArgument {
            message: format!(
                "Step `{step}` error: Wallet resources table must have a header row with columns: {NODE_NAME}, {ACCOUNT_INDEX}, {WALLET_NAME}, {CONNECTED_TO}"
            ),
        });
    }
    // All wallet indexes must be unique
    let account_indexes: HashSet<_> = table
        .rows
        .iter()
        .map(|row| &row[ACCOUNT_INDEX_IDX_T2])
        .collect();
    if account_indexes.len() != table.rows.len() {
        return Err(StepError::InvalidArgument {
            message: format!(
                "Step `{step}` error: Duplicate {ACCOUNT_INDEX} indexes found in the table"
            ),
        });
    }
    // All wallet names must be unique
    let wallet_names: HashSet<_> = table.rows.iter().map(|row| &row[WALLET_NAME_IDX]).collect();
    if wallet_names.len() != table.rows.len() {
        return Err(StepError::InvalidArgument {
            message: format!(
                "Step `{step}` error: Duplicate {WALLET_NAME} indexes found in the table"
            ),
        });
    }
    // node_name and connected_to must be different
    for row in table.rows.iter().skip(1) {
        let node_name = row[NODE_NAME_IDX].trim();
        let connected_to = row
            .get(CONNECTED_TO_IDX)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());
        if let Some(peer) = connected_to
            && peer == node_name
        {
            return Err(StepError::InvalidArgument {
                message: format!(
                    "Step `{step}` error: {NODE_NAME} and {CONNECTED_TO} cannot be the same"
                ),
            });
        }
    }

    Ok(())
}

fn parse_wallet_resources_table_row(
    step: &str,
    row: &[String],
) -> Result<(String, WalletStartInfo, Option<String>), StepError> {
    let node_name = row[NODE_NAME_IDX].trim().to_owned();
    if node_name.is_empty() {
        return Err(StepError::InvalidArgument {
            message: format!("Step `{step}` error: {NODE_NAME} cannot be empty"),
        });
    }
    let account_index = row[ACCOUNT_INDEX_IDX_T2]
        .trim()
        .parse::<usize>()
        .map_err(|_| StepError::InvalidArgument {
            message: format!(
                "Step `{step}` error: {ACCOUNT_INDEX} '{}' must be a valid number",
                row[ACCOUNT_INDEX_IDX_T2]
            ),
        })?;
    let wallet_name = row[WALLET_NAME_IDX].trim().to_owned();
    if wallet_name.is_empty() {
        return Err(StepError::InvalidArgument {
            message: format!("Step `{step}` error: {WALLET_NAME} cannot be empty"),
        });
    }
    let connected_to = row
        .get(CONNECTED_TO_IDX)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    Ok((
        node_name,
        WalletStartInfo {
            wallet_name,
            account_index,
        },
        connected_to,
    ))
}

#[given("I start nodes with wallet resources:")]
#[when("I start nodes with wallet resources:")]
async fn start_nodes_with_wallet_resources(world: &mut CucumberWorld, step: &Step) -> StepResult {
    let table = step
        .table
        .as_ref()
        .ok_or(StepError::MissingTable)
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step.value);
        })?;

    // Map wallet start info and connected peers to node name
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

    let nodes_to_start_ordered = start_nodes_order_respecting_dependencies(nodes_to_start)
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{}` error: {e}", step.value);
        })?;
    for (node_name, wallet_start_info, mut peers) in nodes_to_start_ordered {
        peers.sort();
        peers.dedup();
        start_node(world, &step.value, &node_name, &wallet_start_info, &peers).await?;
    }

    Ok(())
}

// Sort nodes_to_start with empty peers first to ensure standalone nodes start
// before connected nodes, then by dependency order to ensure all peers of a
// node are started before the node itself is started. If there is a circular
// dependency, return an error.
fn start_nodes_order_respecting_dependencies(
    nodes_to_start: NodesToStartUnordered,
) -> Result<NodesToStartOrdered, StepError> {
    let mut remaining = nodes_to_start;
    let mut started = HashSet::new();
    let mut ordered = Vec::new();

    // Step 1: Find all nodes without any peer dependencies
    let nodes_without_peers: Vec<String> = remaining
        .iter()
        .filter(|&(_, (_, peers))| peers.is_empty())
        .map(|(node_name, (_, _))| node_name.clone())
        .collect();

    if nodes_without_peers.is_empty() && !remaining.is_empty() {
        return Err(StepError::InvalidArgument {
            message: "No nodes without peer dependencies found. Possible circular dependency."
                .to_owned(),
        });
    }

    // Update start list with all nodes without peers
    for node_name in nodes_without_peers {
        if let Some((wallet_infos, peers)) = remaining.remove(&node_name) {
            ordered.push((node_name.clone(), wallet_infos, peers));
            started.insert(node_name);
        }
    }

    // Step 2: Iteratively find nodes whose peer dependencies are already included
    // in the start list
    while !remaining.is_empty() {
        let mut made_progress = false;

        let ready_nodes: Vec<String> = remaining
            .iter()
            .filter_map(|(node_name, (_, peers))| {
                let all_peers_started = peers.iter().all(|peer| started.contains(peer));
                all_peers_started.then(|| node_name.clone())
            })
            .collect();

        for node_name in ready_nodes {
            if let Some((wallet_infos, mut peers)) = remaining.remove(&node_name) {
                peers.sort();
                peers.dedup();
                ordered.push((node_name.clone(), wallet_infos, peers));
                started.insert(node_name);
                made_progress = true;
            }
        }

        if !made_progress {
            let remaining_nodes: Vec<String> = remaining.keys().cloned().collect();
            return Err(StepError::InvalidArgument {
                message: format!("Circular dependency detected among nodes: {remaining_nodes:?}"),
            });
        }
    }

    Ok(ordered)
}

pub async fn start_node(
    world: &mut CucumberWorld,
    step: &str,
    node_name: &str,
    wallet_start_info: &[WalletStartInfo],
    peers: &[String],
) -> StepResult {
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
    let mut ibd_peers = HashSet::new();
    for peer in peers {
        if let Some(peer_id) = world.node_peer_ids.get(peer) {
            ibd_peers.insert(*peer_id);
        }
    }
    let is_bootstrap_node = ibd_peers.is_empty();
    let populate_ibd_peers = world.populate_ibd_peers.unwrap_or_default();
    let started_node = Box::pin(
        cluster.start_node_with(
            node_name,
            StartNodeOptions::default()
                .with_peers(peer_selection)
                .with_persist_dir(world.scenario_base_dir.join(node_name))
                .create_patch(move |mut config: RunConfig| {
                    // Placeholder - Add any custom configuration changes here if needed.
                    if !is_bootstrap_node && populate_ibd_peers {
                        config
                            .user
                            .cryptarchia
                            .network
                            .bootstrap
                            .ibd
                            .peers
                            .clone_from(&ibd_peers);
                    }
                    Ok(config)
                }),
        ),
    )
    .await
    .inspect_err(|e| {
        warn!(target: TARGET, "Step `{step}` error: {e}");
    })?;

    // Scrape the final node directory name to get the correct path to the node's
    // YAML file for extracting the peer ID, since the actual directory name has
    // a random suffix added by the deployer.
    let node_final_dir = extract_child_dir_name(&world.scenario_base_dir, &format!("{node_name}_"))
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{step}` error: {e}");
        })?;
    world.node_peer_ids.insert(
        node_name.to_owned(),
        peer_id_from_node_yaml(
            &world
                .scenario_base_dir
                .join(node_final_dir)
                .join(USER_CONFIG_FILE),
        )?,
    );

    let wallet_info = compile_wallet_in_map(wallet_start_info, node_name, world, step)
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{step}` error: {e}");
        })?;

    world
        .wallet_info
        .extend(wallet_info.iter().map(|(k, v)| (k.clone(), v.clone())));
    let started_node_name = started_node.name.clone();
    let client = started_node.client.clone();
    world.nodes_info.insert(
        node_name.to_owned(),
        NodeInfo {
            name: node_name.to_owned(),
            started_node,
            run_config: None,
            chain_info: HashMap::default(),
            wallet_info,
        },
    );

    // Bootstrap peers must be `Mode::OnLine` for IBD of other peers to succeed.
    ensure_node_mode_online(
        cluster,
        &client,
        node_name,
        &started_node_name,
        is_bootstrap_node,
        world
            .require_all_peers_mode_online_at_startup
            .unwrap_or_default(),
    )
    .await
    .inspect_err(|e| {
        warn!(target: TARGET, "Step `{step}` error: {e}");
    })?;

    Ok(())
}

// Ensure this node is ready, and achieved `Mode::OnLine` if it has no IBD peers
// (i.e. a bootstrap node).
async fn ensure_node_mode_online(
    cluster: &LbcManualCluster,
    client: &NodeHttpClient,
    node_name: &str,
    started_node_name: &str,
    is_bootstrap_node: bool,
    require_all_peers_mode_online_at_startup: bool,
) -> StepResult {
    let operation = format!("node '{started_node_name}' readiness");
    track_progress(&operation, Duration::from_secs(5), async {
        cluster
            .wait_node_ready(started_node_name)
            .await
            .map_err(|source| StepError::StepFail {
                message: format!(
                    "node '{started_node_name}' did not become ready after start: {source}"
                ),
            })
    })
    .await?;

    if !is_bootstrap_node && !require_all_peers_mode_online_at_startup {
        return Ok(());
    }

    let start = Instant::now();
    let time_out = Duration::from_secs(60);
    let mut count = 0usize;
    loop {
        match client.consensus_info().await {
            Ok(val) => {
                if val.mode.is_online() {
                    info!(
                        target: TARGET,
                        "Node `{node_name}/{started_node_name}` achieved `Mode::OnLine` in {:.2?}",
                        start.elapsed()
                    );
                    return Ok(());
                }
            }
            Err(e) if start.elapsed() < time_out => {
                if count.is_multiple_of(20) {
                    info!(
                        target: TARGET,
                        "Waiting for node `{node_name}/{started_node_name}` to be `Mode::OnLine` - elapsed: {:.2?} ({e})",
                        start.elapsed()
                    );
                }
            }
            Err(e) => {
                return Err(StepError::StepFail {
                    message: format!(
                        "Node `{node_name}/{started_node_name}` failed `Mode::OnLine` - elapsed {:.2?}: {e}",
                        start.elapsed()
                    ),
                });
            }
        }
        sleep(Duration::from_millis(100)).await;
        count += 1;
    }
}

fn compile_wallet_in_map(
    wallet_start_info: &[WalletStartInfo],
    node_name: &str,
    world: &CucumberWorld,
    step: &str,
) -> Result<WalletInfoMap, StepError> {
    let mut wallet_info: WalletInfoMap = HashMap::new();
    for wallet in wallet_start_info {
        let wallet_account = world
            .wallet_accounts
            .get(&wallet.account_index)
            .ok_or(StepError::LogicalError {
                message: format!(
                    "Step `{step}` error: Wallet account with index {} not found",
                    wallet.account_index
                ),
            })?
            .clone();
        wallet_info.insert(
            wallet.wallet_name.clone(),
            WalletInfo {
                wallet_name: wallet.wallet_name.clone(),
                node_name: node_name.to_owned(),
                account_index: wallet.account_index,
                wallet_account,
            },
        );
    }
    Ok(wallet_info)
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
    step: &str,
    nodes_info: &mut HashMap<String, NodeInfo>,
    nodes_chain_info: &mut HashMap<String, ChainInfoMap>,
) -> Result<(u64, u64, Vec<u64>), StepError> {
    poll_all_nodes_and_update_consensus_cache(step, nodes_info).await?;

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
            let fork_hashes: HashSet<_> = anchor_hashes
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

pub async fn nodes_converged(
    world: &mut CucumberWorld,
    step: &str,
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
            fetch_and_update_chain_info(step, &mut world.nodes_info, &mut nodes_chain_info).await?;
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
                    "Step `{step}` error: Nodes did not converge to {max_diff_height} blocks at in \
                    {time_out_seconds} s"
                ),
            }, |min_height| StepError::StepFail {
                message: format!(
                    "Step `{step}` error: Nodes did not converge to {max_diff_height} blocks at minimum height \
                    {min_height} in {time_out_seconds} s"
                ),
            });
            return Err(err);
        }

        sleep(Duration::from_millis(100)).await;
        count += 1;
    }
}

pub async fn poll_all_nodes_and_update_consensus_cache<S: ::std::hash::BuildHasher>(
    step: &str,
    nodes_info: &mut HashMap<String, NodeInfo, S>,
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
            "Step `{step}` error: Some node(s) did not respond with their consensus_info: {e}",
        );
    })?;

    for snap in &snapshots {
        let node = nodes_info
            .get_mut(&snap.node_name)
            .ok_or(StepError::LogicalError {
                message: format!(
                    "Step `{step}` error: Runtime node '{}' not found in world.nodes_info",
                    snap.node_name
                ),
            })?;
        node.upsert_tip(snap.height, snap.header_hash.clone());
    }

    Ok(())
}

// This struct represents the wallet resources to be associated with a node at
// startup.
pub struct WalletStartInfo {
    // Logical name of the wallet resource, used for referencing in steps.
    pub wallet_name: String,
    // The account index in the genesis tokens that this resource corresponds to.
    pub account_index: usize,
}
