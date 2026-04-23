use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use cucumber::gherkin::Table;
use futures_util::future::try_join_all;
use hex::ToHex as _;
use lb_chain_service::CryptarchiaInfo;
use lb_core::mantle::{GenesisTx as _, Utxo, ops::OpId as _};
use lb_http_api_common::paths::CRYPTARCHIA_INFO;
use lb_libp2p::PeerId;
use lb_node::config::{DeploymentSettings, RunConfig, WellKnownDeployment};
use lb_testing_framework::{
    LbcEnv, LbcManualCluster, NodeHttpClient, USER_CONFIG_FILE, configs::wallet::WalletAccount,
};
use libp2p::Multiaddr;
use reqwest::{Client, Url};
use testing_framework_core::scenario::{PeerSelection, StartNodeOptions, StartedNode};
use tokio::time::{Instant, sleep};
use tracing::{info, warn};

use crate::cucumber::{
    error::{StepError, StepResult},
    steps::{
        TARGET,
        manual_nodes::{
            config_override::apply_user_config_overrides,
            snapshots::{
                restore_node_state_from_snapshot, save_named_blockchain_snapshot,
                validate_snapshot_path_component,
            },
        },
    },
    utils::{
        display_last_path_components, extract_child_dir_name, funding_wallet_pk_from_node_yaml,
        matching_child_dirs, peer_id_from_node_yaml, track_progress, truncate_hash,
    },
    world::{
        ChainInfoMap, CucumberWorld, ManualNodeConfigOverrides, NodeInfo,
        PublicCryptarchiaEndpointPeer, UserConfigOverride, WalletInfo, WalletInfoMap, WalletType,
    },
};

pub(crate) type NodesToStartUnordered = HashMap<String, (Vec<WalletStartInfo>, Vec<String>)>;
type NodesToStartOrdered = Vec<(String, Vec<WalletStartInfo>, Vec<String>)>;

const CHAIN_SYNC_POLL_INTERVAL: Duration = Duration::from_secs(5);
const CHAIN_SYNC_STATUS_LOG_INTERVAL: Duration = Duration::from_mins(2);

// Returns the root directory for a named snapshot.

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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SyncTargetStats {
    lib: String,
    tip: String,
    slot: u64,
    height: u64,
}

#[derive(Debug, Clone)]
struct PublicPeerConsensusSnapshot {
    peer_url: String,
    stats: SyncTargetStats,
}

#[derive(Debug, Clone)]
struct MajorityPublicSyncTarget {
    peer_urls: Vec<String>,
    stats: SyncTargetStats,
}

impl SyncTargetStats {
    fn from_cryptarchia_info(info: &CryptarchiaInfo) -> Self {
        Self {
            lib: info.lib.encode_hex::<String>(),
            tip: info.tip.encode_hex::<String>(),
            slot: info.slot.into_inner(),
            height: info.height,
        }
    }
}

#[must_use]
pub(crate) fn genesis_block_utxos(
    genesis_tx: &lb_core::mantle::genesis_tx::GenesisTx,
) -> Vec<Utxo> {
    let transfer_op = genesis_tx.genesis_transfer().clone();
    let transfer_id = transfer_op.op_id();

    transfer_op
        .outputs
        .iter()
        .enumerate()
        .map(|(idx, note)| Utxo::new(transfer_id, idx, *note))
        .collect()
}

const ACCOUNT_INDEX: &str = "account_index";
const ACCOUNT_INDEX_IDX_T1: usize = 0;
const TOKEN_COUNT: &str = "token_count";
const TOKEN_COUNT_IDX: usize = 1;
const TOKEN_AMOUNT: &str = "token_amount";
const TOKEN_AMOUNT_IDX: usize = 2;

pub(crate) fn verify_genesis_wallet_resources_table_indexes(
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

pub(crate) fn parse_genesis_wallet_tokens_row(
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

const NODE_NAME: &str = "node_name";
const NODE_NAME_IDX: usize = 0;
const ACCOUNT_INDEX_IDX_T2: usize = 1;
const WALLET_NAME: &str = "wallet_name";
const WALLET_NAME_IDX: usize = 2;
const CONNECTED_TO: &str = "connected_to";
const CONNECTED_TO_IDX: usize = 3;

pub(crate) fn verify_node_wallet_resources_table_indexes(
    table: &Table,
    step: &str,
) -> Result<(), StepError> {
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

pub(crate) fn parse_wallet_resources_table_row(
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
        .map(str::to_owned);

    Ok((
        node_name,
        WalletStartInfo {
            wallet_name,
            account_index,
        },
        connected_to,
    ))
}

pub(crate) fn ensure_fee_sponsorship_and_fork_groups_are_not_mixed(
    world: &CucumberWorld,
    step_value: &str,
) -> StepResult {
    if world.fee_state.sponsored_genesis_account.is_some() && !world.node_groups.is_empty() {
        return Err(StepError::InvalidArgument {
            message: format!(
                "Step `{step_value}` error: sponsored fee accounts cannot be combined with distinct node groups in the same scenario"
            ),
        });
    }

    Ok(())
}

pub(crate) async fn wait_for_all_nodes_to_be_synced_to_chain(
    world: &CucumberWorld,
    step: &str,
) -> StepResult {
    let public_cryptarchia_endpoint_peers = world
        .public_cryptarchia_endpoint_peers
        .clone()
        .unwrap_or_default();
    if public_cryptarchia_endpoint_peers.is_empty() {
        return Err(StepError::InvalidArgument {
            message: format!(
                "Step `{step}` error: no public cryptarchia endpoint peers configured"
            ),
        });
    }
    if world.nodes_info.is_empty() {
        return Err(StepError::InvalidArgument {
            message: format!("Step `{step}` error: no local nodes are available to check sync"),
        });
    }

    let client = Client::new();
    let start = Instant::now();
    let mut last_status_log_at = None;

    loop {
        let public_snapshots =
            fetch_public_peer_consensus_snapshots(&client, &public_cryptarchia_endpoint_peers)
                .await;
        let majority_target = select_majority_public_sync_target(&public_snapshots);

        if let Some(target) = majority_target.as_ref()
            && all_local_nodes_match_sync_target(world, target).await
        {
            get_cryptarchia_info_all_nodes(world, step).await;
            info!(
                target: TARGET,
                "All nodes synced to the chain in {:.2?}",
                start.elapsed()
            );
            return Ok(());
        }

        if should_log_chain_sync_status(last_status_log_at) {
            log_chain_sync_progress(
                start.elapsed(),
                public_cryptarchia_endpoint_peers.len(),
                &public_snapshots,
                majority_target.as_ref(),
            );
            get_cryptarchia_info_all_nodes(world, step).await;
            last_status_log_at = Some(Instant::now());
        }

        sleep(CHAIN_SYNC_POLL_INTERVAL).await;
    }
}

pub(crate) fn parse_url(raw: &str) -> Result<String, String> {
    let mut trimmed = raw.trim();
    trimmed = trimmed.trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("url cannot be empty".to_owned());
    }

    Url::parse(trimmed).map_err(|e| format!("invalid url '{trimmed}': {e}"))?;

    Ok(trimmed.to_owned())
}

async fn fetch_public_peer_consensus_snapshots(
    client: &Client,
    peers: &[PublicCryptarchiaEndpointPeer],
) -> Vec<PublicPeerConsensusSnapshot> {
    let mut snapshots = Vec::new();

    for peer in peers {
        match fetch_public_peer_consensus(client, peer).await {
            Ok(info) => snapshots.push(PublicPeerConsensusSnapshot {
                peer_url: peer.url.clone(),
                stats: SyncTargetStats::from_cryptarchia_info(&info),
            }),
            Err(e) => warn!(
                target: TARGET,
                "Failed to fetch public cryptarchia info from '{}': {e}",
                peer.url
            ),
        }
    }

    snapshots
}

async fn fetch_public_peer_consensus(
    client: &Client,
    peer: &PublicCryptarchiaEndpointPeer,
) -> Result<CryptarchiaInfo, StepError> {
    let request_url = Url::parse(&format!(
        "{peer_url}/{path}",
        peer_url = peer.url.as_str(),
        path = CRYPTARCHIA_INFO.trim_start_matches('/')
    ))
    .map_err(|e| StepError::InvalidArgument {
        message: format!(
            "Invalid public cryptarchia info URL for '{}': {e}",
            peer.url.as_str()
        ),
    })?;

    client
        .get(request_url)
        .basic_auth(&peer.username, Some(&peer.password))
        .send()
        .await?
        .error_for_status()?
        .json::<CryptarchiaInfo>()
        .await
        .map_err(Into::into)
}

fn select_majority_public_sync_target(
    snapshots: &[PublicPeerConsensusSnapshot],
) -> Option<MajorityPublicSyncTarget> {
    let mut groups = HashMap::<SyncTargetStats, Vec<String>>::new();
    for snapshot in snapshots {
        groups
            .entry(snapshot.stats.clone())
            .or_default()
            .push(snapshot.peer_url.clone());
    }

    let best = groups
        .into_iter()
        .max_by(|(left_stats, left_peers), (right_stats, right_peers)| {
            left_peers
                .len()
                .cmp(&right_peers.len())
                .then_with(|| left_stats.height.cmp(&right_stats.height))
                .then_with(|| left_stats.slot.cmp(&right_stats.slot))
        })
        .map(|(stats, peer_urls)| MajorityPublicSyncTarget { peer_urls, stats })?;

    if best.peer_urls.len() * 2 <= snapshots.len() {
        return None;
    }

    Some(best)
}

async fn all_local_nodes_match_sync_target(
    world: &CucumberWorld,
    target: &MajorityPublicSyncTarget,
) -> bool {
    let mut node_names = world.nodes_info.keys().cloned().collect::<Vec<_>>();
    node_names.sort();

    for node_name in node_names {
        let Some(node_info) = world.nodes_info.get(&node_name) else {
            return false;
        };

        let Ok(consensus) = node_info.started_node.client.consensus_info().await else {
            return false;
        };
        if SyncTargetStats::from_cryptarchia_info(&consensus) != target.stats {
            return false;
        }
    }

    true
}

fn should_log_chain_sync_status(last_status_log_at: Option<Instant>) -> bool {
    last_status_log_at.is_none_or(|last| last.elapsed() >= CHAIN_SYNC_STATUS_LOG_INTERVAL)
}

fn log_chain_sync_progress(
    elapsed: Duration,
    total_public_peers: usize,
    public_snapshots: &[PublicPeerConsensusSnapshot],
    majority_target: Option<&MajorityPublicSyncTarget>,
) {
    if let Some(target) = majority_target {
        info!(
            target: TARGET,
            "Waiting to be synced - elapsed {:.2?}, height {}/{}, public peers {}/{}, majority {}/{}, tip '{} ...', lib '{} ...'",
            elapsed,
            target.stats.height,
            target.stats.slot,
            public_snapshots.len(),
            total_public_peers,
            target.peer_urls.len(),
            public_snapshots.len(),
            truncate_hash(&target.stats.tip, 16),
            truncate_hash(&target.stats.lib, 16),
        );
    } else {
        info!(
            target: TARGET,
            "Waiting to be synced - elapsed {:.2?}, no majority public peer consensus ({}/{} reachable)",
            elapsed,
            public_snapshots.len(),
            total_public_peers,
        );
    }
}

// Sort nodes_to_start with empty peers first to ensure standalone nodes start
// before connected nodes, then by dependency order to ensure all peers of a
// node are started before the node itself is started. If there is a circular
// dependency, return an error.
pub(crate) fn start_nodes_order_respecting_dependencies(
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
        if let Some((wallet_infos, initial_peers)) = remaining.remove(&node_name) {
            ordered.push((node_name.clone(), wallet_infos, initial_peers));
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

#[expect(
    clippy::too_many_lines,
    reason = "Covers startup, optional snapshot seeding, wallet wiring, and readiness in one path"
)]
#[expect(
    clippy::cognitive_complexity,
    reason = "Singular fn with multiple branches to handle different events and futures."
)]
pub async fn start_node(
    world: &mut CucumberWorld,
    step: &str,
    node_name: &str,
    wallet_start_info: &[WalletStartInfo],
    initial_peers: &[String],
) -> StepResult {
    let cluster = world
        .local_cluster
        .as_ref()
        .ok_or(StepError::LogicalError {
            message: "No local cluster available".into(),
        })?;
    let startup_settings = get_startup_settings(world, initial_peers).inspect_err(|e| {
        warn!(target: TARGET, "Step `{step}` error: {e}");
    })?;
    let is_bootstrap_node = startup_settings.is_bootstrap_node;
    let join_external_network = startup_settings.join_external_network;
    let persist_dir = world.scenario_base_dir.join(node_name);
    let runtime_dir_prefix = format!("{node_name}_");
    let final_dir_ignore_list = matching_child_dirs(&persist_dir, &runtime_dir_prefix);
    let started_node = Box::pin(
        cluster.start_node_with(
            node_name,
            StartNodeOptions::default()
                .with_peers(startup_settings.peer_selection)
                .with_persist_dir(persist_dir)
                .create_patch(move |mut config: RunConfig| {
                    prepare_config_patch(
                        &mut config,
                        startup_settings.join_external_network,
                        startup_settings.deployment_override.as_ref(),
                        &startup_settings.config_overrides,
                        startup_settings.initial_peers_override.as_ref(),
                        &startup_settings.ibd_peers,
                        &startup_settings.user_config_overrides,
                    )?;
                    Ok(config)
                }),
        ),
    )
    .await
    .inspect_err(|e| {
        warn!(target: TARGET, "Step `{step}` error: {e}");
    })?;

    let node_final_dir = extract_child_dir_name(
        &world.scenario_base_dir,
        &runtime_dir_prefix,
        &final_dir_ignore_list,
    )
    .inspect_err(|e| {
        warn!(target: TARGET, "Step `{step}` error: {e}");
    })?;
    let node_runtime_dir = world.scenario_base_dir.join(node_final_dir.clone());
    let started_node_name = started_node.name.clone();
    info!(
        target: TARGET,
        "Starting node `{node_name}` with runtime_dir='{}'",
        display_last_path_components(&node_runtime_dir, 4)
    );

    // `StartNodeOptions::with_persist_dir` currently creates a fresh runtime
    // directory for each launch. Seed that runtime directory and restart once
    // to effectively initialize from a named snapshot.
    if let Some(node_snapshot) = world.blockchain_snapshot_on_startup.as_ref() {
        cluster
            .stop_node(&started_node_name)
            .await
            .inspect_err(|e| {
                warn!(target: TARGET, "Step `{step}` error: {e}");
            })?;
        restore_node_state_from_snapshot(node_snapshot, &node_runtime_dir).inspect_err(|e| {
            warn!(target: TARGET, "Step `{step}` error: {e}");
        })?;
        cluster
            .restart_node(&started_node_name)
            .await
            .inspect_err(|e| {
                warn!(target: TARGET, "Step `{step}` error: {e}");
            })?;
        info!(
            target: TARGET,
            "Node {node_name} started from snapshot {}/{}",
            node_snapshot.name, node_snapshot.node
        );
    }

    // Scrape the final node directory name to get the correct path to the node's
    // YAML file for extracting the peer ID, since the actual directory name has
    // a random suffix added by the deployer.
    world.node_peer_ids.insert(
        node_name.to_owned(),
        peer_id_from_node_yaml(&node_runtime_dir.join(USER_CONFIG_FILE))?,
    );

    let wallet_info = add_wallets(
        world,
        step,
        node_name,
        wallet_start_info,
        &started_node,
        &node_runtime_dir,
        join_external_network,
    )
    .inspect_err(|e| {
        warn!(target: TARGET, "Step `{step}` error: {e}");
    })?;

    world
        .wallet_info
        .extend(wallet_info.iter().map(|(k, v)| (k.clone(), v.clone())));

    let client = started_node.client.clone();
    // Move `started_node` into the world's NodeInfo (no clone required)
    world.nodes_info.insert(
        node_name.to_owned(),
        NodeInfo {
            name: node_name.to_owned(),
            started_node,
            run_config: None,
            chain_info: HashMap::default(),
            wallet_info,
            runtime_dir: node_runtime_dir,
        },
    );

    // Bootstrap peers must be `Mode::OnLine` for IBD of other peers to succeed.
    ensure_node_ready(
        cluster,
        &client,
        node_name,
        &started_node_name,
        is_bootstrap_node,
        world.require_all_peers_mode_online_at_startup,
        startup_settings.join_external_network,
    )
    .await
    .inspect_err(|e| {
        warn!(target: TARGET, "Step `{step}` error: {e}");
    })?;

    if world.blockchain_snapshot_on_startup.is_some() {
        match client.consensus_info().await {
            Ok(info) => {
                info!(
                    target: TARGET,
                    "Node `{node_name}` snapshot state - height: {}/{}, tip: {}, lib: {}",
                    info.height,
                    info.slot.into_inner(),
                    truncate_hash(&info.tip.encode_hex::<String>(), 16),
                    truncate_hash(&info.lib.encode_hex::<String>(), 16)
                );
            }
            Err(e) => {
                warn!(
                    target: TARGET,
                    "Node `{node_name}` failed to fetch post-start consensus after snapshot init: {e}"
                );
            }
        }
    }

    Ok(())
}

pub async fn restart_node(world: &CucumberWorld, step: &str, node_name: &str) -> StepResult {
    let cluster = world
        .local_cluster
        .as_ref()
        .ok_or(StepError::LogicalError {
            message: "No local cluster available".into(),
        })?;
    let started_node_name = world
        .resolve_node_runtime_name(node_name)
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{step}` error: {e}");
        })?;

    cluster
        .restart_node(&started_node_name)
        .await
        .inspect_err(|e| {
            warn!(target: TARGET, "Step `{step}` error: {e}");
        })?;
    let client = world.resolve_node_http_client(node_name).inspect_err(|e| {
        warn!(target: TARGET, "Step `{step}` error: {e}");
    })?;
    ensure_node_ready(
        cluster,
        &client,
        node_name,
        &started_node_name,
        // TODO: Add `is_bootstrap_node` to world
        false,
        None,
        world.join_external_network.unwrap_or_default(),
    )
    .await
    .inspect_err(|e| {
        warn!(target: TARGET, "Step `{step}` error: {e}");
    })?;

    info!(
        target: TARGET,
        "Restarted node `{node_name}` (runtime name `{started_node_name}`)"
    );

    Ok(())
}

fn add_wallets(
    world: &CucumberWorld,
    step: &str,
    node_name: &str,
    wallet_start_info: &[WalletStartInfo],
    started_node: &StartedNode<LbcEnv>,
    node_runtime_dir: &Path,
    join_external_network: bool,
) -> Result<WalletInfoMap, StepError> {
    let wallet_info = compile_wallet_in_map(
        wallet_start_info,
        node_name,
        world,
        step,
        node_runtime_dir,
        join_external_network,
    )?;
    for (wallet_name, info) in &wallet_info {
        let wallet_type = match info.wallet_type.clone() {
            WalletType::User { .. } => "User",
            WalletType::Funding { .. } => "Funding",
        };
        info!(target: TARGET, "{wallet_type} wallet `{}/{node_name}` created: {}",
           wallet_name,
           format!("{}wallet/{}/balance", started_node.client.base_url(), info.public_key_hex())
        );
    }

    Ok(wallet_info)
}

struct StartupSettings {
    peer_selection: PeerSelection,
    ibd_peers: HashSet<PeerId>,
    is_bootstrap_node: bool,
    initial_peers_override: Option<Vec<Multiaddr>>,
    join_external_network: bool,
    deployment_override: Option<DeploymentSettings>,
    config_overrides: ManualNodeConfigOverrides,
    user_config_overrides: Vec<UserConfigOverride>,
}

fn get_startup_settings(
    world: &CucumberWorld,
    initial_peers: &[String],
) -> Result<StartupSettings, StepError> {
    let peer_selection = if initial_peers.is_empty() {
        PeerSelection::None
    } else {
        let named = initial_peers
            .iter()
            .map(|peer| world.resolve_node_runtime_name(peer))
            .collect::<Result<Vec<String>, StepError>>()?;
        PeerSelection::Named(named)
    };
    let mut ibd_peers = world.ibd_peers_override.clone().unwrap_or_default();
    let populate_ibd_peers_from_initial_peers = world
        .populate_ibd_peers_from_initial_peers
        .unwrap_or_default();
    if populate_ibd_peers_from_initial_peers {
        for peer in initial_peers {
            if let Some(peer_id) = world.node_peer_ids.get(peer) {
                ibd_peers.insert(*peer_id);
            }
        }
    }
    let is_bootstrap_node = initial_peers.is_empty();
    let initial_peers_override = world.initial_peers_override.clone();
    let join_external_network = world.join_external_network.unwrap_or_default();
    let deployment_override = world
        .deployment_config_override_path
        .clone()
        .map(|path| load_run_config(&path))
        .transpose()?;
    let user_config_overrides = world.user_config_overrides.clone();

    Ok(StartupSettings {
        peer_selection,
        ibd_peers,
        is_bootstrap_node,
        initial_peers_override,
        join_external_network,
        deployment_override,
        config_overrides: world.manual_node_config_overrides.clone(),
        user_config_overrides,
    })
}

fn prepare_config_patch(
    config: &mut RunConfig,
    join_external_network: bool,
    deployment_override: Option<&DeploymentSettings>,
    config_overrides: &ManualNodeConfigOverrides,
    initial_peers_override: Option<&Vec<Multiaddr>>,
    ibd_peers: &HashSet<PeerId>,
    user_config_overrides: &[UserConfigOverride],
) -> Result<(), StepError> {
    if join_external_network {
        config.deployment = deployment_override
            .cloned()
            .unwrap_or_else(|| DeploymentSettings::from(WellKnownDeployment::Devnet));
    } else if let Some(deployment_override) = deployment_override {
        config.deployment = deployment_override.clone();
    }

    config_overrides.apply_to(config);

    if let Some(initial_peers) = &initial_peers_override {
        config
            .user
            .network
            .backend
            .initial_peers
            .clone_from(initial_peers);
    }
    config
        .user
        .cryptarchia
        .network
        .bootstrap
        .ibd
        .peers
        .clone_from(ibd_peers);

    apply_user_config_overrides(config, user_config_overrides)?;
    Ok(())
}

fn load_run_config(path: &Path) -> Result<DeploymentSettings, StepError> {
    let text = fs::read_to_string(path).map_err(|e| StepError::LogicalError {
        message: format!("Failed to read '{}': {e}", path.display()),
    })?;
    serde_yaml::from_str::<DeploymentSettings>(&text).map_err(|e| StepError::LogicalError {
        message: format!("Failed to parse '{}': {e}", path.display()),
    })
}

// Ensure this node is ready, and achieved `Mode::OnLine` if it is a bootstrap
// node.
async fn ensure_node_ready(
    cluster: &LbcManualCluster,
    client: &NodeHttpClient,
    node_name: &str,
    started_node_name: &str,
    is_bootstrap_node: bool,
    require_all_peers_mode_online_at_startup: Option<Duration>,
    join_external_network: bool,
) -> StepResult {
    // General readiness check to ensure the node is responsive.
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

    verify_reponsive_and_network_ready(client, node_name, started_node_name).await?;

    if !is_bootstrap_node && require_all_peers_mode_online_at_startup.is_none()
        || join_external_network
    {
        return Ok(());
    }

    verify_online(
        client,
        node_name,
        started_node_name,
        require_all_peers_mode_online_at_startup,
    )
    .await?;
    Ok(())
}

async fn verify_online(
    client: &NodeHttpClient,
    node_name: &str,
    started_node_name: &str,
    time_out: Option<Duration>,
) -> StepResult {
    let time_out = time_out.unwrap_or_else(|| Duration::from_mins(1));
    let start = Instant::now();
    let mut count = 0usize;
    loop {
        let mut mode_online = false;
        match client.consensus_info().await {
            Ok(val) => {
                if val.mode.is_online() {
                    mode_online = true;
                }
            }
            Err(e) if start.elapsed() < time_out => {
                if count.is_multiple_of(20) {
                    info!(
                        target: TARGET,
                        "Waiting for node `{node_name}/{started_node_name}` to be `Mode::OnLine` - \
                         elapsed: {:.2?} ({e})",
                        start.elapsed()
                    );
                }
            }
            Err(e) => {
                return Err(StepError::StepFail {
                    message: format!(
                        "Node `{node_name}/{started_node_name}` failed `Mode::OnLine` - elapsed \
                        {:.2?}: {e}",
                        start.elapsed()
                    ),
                });
            }
        }
        if mode_online {
            info!(
                target: TARGET,
                "Node `{node_name}/{started_node_name}` achieved `Mode::OnLine` and listen \
                addresses in {:.2?}",
                start.elapsed()
            );
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
        count += 1;
    }
}

#[expect(
    clippy::cognitive_complexity,
    reason = "Singular fn with multiple branches to handle different events and futures."
)]
async fn verify_reponsive_and_network_ready(
    client: &NodeHttpClient,
    node_name: &str,
    started_node_name: &str,
) -> StepResult {
    let start = Instant::now();
    let time_out = Duration::from_mins(1);
    let mut count = 0usize;
    let mut can_provide_consensus_info;
    let mut is_network_ready;

    loop {
        can_provide_consensus_info = false;
        match client.consensus_info().await {
            Ok(_) => {
                can_provide_consensus_info = true;
            }
            Err(e) if start.elapsed() < time_out => {
                if count.is_multiple_of(20) {
                    info!(
                        target: TARGET,
                        "Waiting for node `{node_name}/{started_node_name}` to be responsive - \
                         elapsed: {:.2?} ({e})",
                        start.elapsed()
                    );
                }
            }
            Err(e) => {
                return Err(StepError::StepFail {
                    message: format!(
                        "Node `{node_name}/{started_node_name}` failed to be responsive - elapsed \
                        {:.2?}: {e}",
                        start.elapsed()
                    ),
                });
            }
        }
        is_network_ready = false;
        match client.network_info().await {
            Ok(val) => {
                is_network_ready = !val.listen_addresses.is_empty();
            }
            Err(e) if start.elapsed() < time_out => {
                if count.is_multiple_of(20) {
                    info!(
                        target: TARGET,
                        "Waiting for node `{node_name}/{started_node_name}` to be network ready - \
                        elapsed: {:.2?} ({e})",
                        start.elapsed()
                    );
                }
            }
            Err(e) => {
                return Err(StepError::StepFail {
                    message: format!(
                        "Node `{node_name}/{started_node_name}` failed to be network ready - elapsed \
                        {:.2?}: {e}",
                        start.elapsed()
                    ),
                });
            }
        }
        if can_provide_consensus_info && is_network_ready {
            info!(
                target: TARGET,
                "Node `{node_name}/{started_node_name}` is responsive and network ready in {:.2?}",
                start.elapsed()
            );
            return Ok(());
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
    node_runtime_dir: &Path,
    join_external_network: bool,
) -> Result<WalletInfoMap, StepError> {
    let mut wallet_info: WalletInfoMap = HashMap::new();
    for wallet in wallet_start_info {
        let wallet_account = match world.wallet_accounts.get(&wallet.account_index) {
            Some(wallet_account) => wallet_account.clone(),
            None => {
                if join_external_network {
                    WalletAccount::random()
                        .map_err(|source| StepError::LogicalError {
                            message: format!(
                                "Step `{step}` error: Failed to derive random wallet account for index {}: {source}",
                                wallet.account_index
                            ),
                        })?
                } else {
                    WalletAccount::deterministic(
                        wallet.account_index as u64,
                        0,
                        true,
                    )
                        .map_err(|source| StepError::LogicalError {
                            message: format!(
                                "Step `{step}` error: Failed to derive deterministic wallet account for index {}: {source}",
                                wallet.account_index
                            ),
                        })?
                }
            }
        };

        wallet_info.insert(
            wallet.wallet_name.clone(),
            WalletInfo {
                wallet_name: wallet.wallet_name.clone(),
                node_name: node_name.to_owned(),
                wallet_type: WalletType::User { wallet_account },
            },
        );
    }

    let funding_wallet_name = format!("{node_name}_WALLET");
    wallet_info.insert(
        funding_wallet_name.clone(),
        WalletInfo {
            wallet_name: funding_wallet_name,
            node_name: node_name.to_owned(),
            wallet_type: WalletType::Funding {
                wallet_pk: funding_wallet_pk_from_node_yaml(
                    &node_runtime_dir.join(USER_CONFIG_FILE),
                )?,
            },
        },
    );

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

pub async fn ensure_all_nodes_agree_on_lib(
    world: &CucumberWorld,
    step: &str,
    time_out_seconds: u64,
) -> StepResult {
    let start = Instant::now();
    let time_out = Duration::from_secs(time_out_seconds);
    let mut count = 0usize;

    loop {
        let snapshots = try_join_all(world.nodes_info.values().map(async |node| {
            let consensus = node.started_node.client.consensus_info().await?;
            Ok::<_, StepError>((
                node.name.clone(),
                consensus.height,
                consensus.lib.encode_hex::<String>(),
            ))
        }))
        .await?;

        let libs = snapshots
            .iter()
            .map(|(_, _, lib)| lib.clone())
            .collect::<HashSet<_>>();

        if libs.len() == 1 {
            info!(
                target: TARGET,
                "All nodes agree on LIB in {:.2?}",
                start.elapsed()
            );
            return Ok(());
        }

        if count.is_multiple_of(50) {
            let status = format_lib_agreement_status(&snapshots);

            info!(
                target: TARGET,
                "Waiting for all nodes to agree on LIB - elapsed {:.2?}, {status}",
                start.elapsed()
            );
        }

        if start.elapsed() >= time_out {
            let status = format_lib_agreement_status(&snapshots);

            return Err(StepError::StepFail {
                message: format!(
                    "Step `{step}` error: Nodes did not agree on LIB in {time_out_seconds} s ({status})"
                ),
            });
        }

        sleep(Duration::from_millis(100)).await;
        count += 1;
    }
}

fn format_lib_agreement_status(snapshots: &[(String, u64, String)]) -> String {
    snapshots
        .iter()
        .map(|(node_name, height, lib)| format!("{node_name}: {height}/{}", truncate_hash(lib, 16)))
        .collect::<Vec<_>>()
        .join(", ")
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

/// This struct represents the wallet resources to be associated with a node at
/// startup.
pub struct WalletStartInfo {
    // Logical name of the wallet resource, used for referencing in steps.
    pub wallet_name: String,
    // The account index in the genesis tokens that this resource corresponds to.
    pub account_index: usize,
}

/// Saves the current blockchain state of all nodes into a named snapshot
/// location for later use.
pub fn create_snapshots_all_nodes(
    world: &CucumberWorld,
    snapshot_name: &str,
) -> Result<(), StepError> {
    validate_snapshot_path_component(snapshot_name, "Snapshot name")?;

    let runtime_dir_by_node_name: Vec<(String, PathBuf)> = world
        .nodes_info
        .iter()
        .map(|(node_name, info)| (node_name.clone(), info.runtime_dir.clone()))
        .collect();

    for (node_name, node_runtime_dir) in &runtime_dir_by_node_name {
        save_named_blockchain_snapshot(snapshot_name, node_name, node_runtime_dir)?;
        info!(
            target: TARGET,
            "Saved blockchain snapshot `{snapshot_name}` for node `{node_name}`",
        );
    }
    Ok(())
}

/// Fetches and logs the consensus info of all nodes, for debugging purposes.
/// Does not require the nodes to be aligned or have any specific state, and is
/// resilient to some nodes being offline or unresponsive.
#[expect(
    clippy::cognitive_complexity,
    reason = "Singular fn with multiple branches to handle different events and futures."
)]
pub(crate) async fn get_cryptarchia_info_all_nodes(world: &CucumberWorld, step: &str) {
    let mut node_names = world.nodes_info.keys().cloned().collect::<Vec<_>>();
    node_names.sort();

    if node_names.is_empty() {
        warn!(
            target: TARGET,
            "Step `{step}` no nodes found for CRYPTARCHIA_INFO_ALL_NODES"
        );
        return;
    }

    for node_name in node_names {
        let Some(node_info) = world.nodes_info.get(&node_name) else {
            continue;
        };
        match node_info.started_node.client.consensus_info().await {
            Ok(consensus) => {
                let mode = if consensus.mode.is_online() {
                    "Online"
                } else {
                    "Bootstrapping"
                };
                info!(
                    target: TARGET,
                    "cryptarchia/info - '{}', '{}', {}/{}, tip '{} ...', lib '{} ...'",
                    node_name,
                    mode,
                    consensus.height,
                    consensus.slot.into_inner(),
                    truncate_hash(&consensus.tip.encode_hex::<String>(), 16),
                    truncate_hash(&consensus.lib.encode_hex::<String>(), 16),
                );
            }
            Err(e) => {
                warn!(
                    target: TARGET,
                    "Step `{step}` CRYPTARCHIA_INFO failed for node `{node_name}`: {e}",
                );
            }
        }
    }
}
