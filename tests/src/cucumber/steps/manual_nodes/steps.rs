use std::{collections::HashMap, time::Duration};

use cucumber::{gherkin::Step, given, then, when};
use lb_libp2p::{Multiaddr, PeerId};
use tokio::time::{Instant, sleep};
use tracing::{info, warn};

use crate::{
    cucumber::{
        error::{StepError, StepResult},
        steps::{
            TARGET,
            manual_cluster::{
                assert_manual_node_has_peers, connect_manual_node_to_node,
                install_local_manual_cluster, rebuild_pending_local_manual_cluster,
                stop_active_manual_cluster,
            },
            manual_nodes::{
                config_override::set_user_config_override,
                snapshots::{save_named_blockchain_snapshot, validate_snapshot_path_component},
                utils::{
                    NodesToStartUnordered, create_snapshots_all_nodes,
                    ensure_all_nodes_agree_on_lib,
                    ensure_fee_sponsorship_and_fork_groups_are_not_mixed,
                    get_cryptarchia_info_all_nodes, nodes_converged,
                    parse_genesis_wallet_tokens_row, parse_url, parse_wallet_resources_table_row,
                    poll_all_nodes_and_update_consensus_cache, restart_node, start_node,
                    start_nodes_order_respecting_dependencies,
                    verify_genesis_wallet_resources_table_indexes,
                    verify_node_wallet_resources_table_indexes,
                    wait_for_all_nodes_to_be_synced_to_chain,
                },
            },
        },
        utils::resolve_literal_or_env,
        world::{
            CucumberWorld, GenesisTokens, ManualClusterKind, ManualClusterSpec, NodeSnapshot,
            PublicCryptarchiaEndpointPeer,
        },
    },
    non_zero,
};

const PUBLIC_CRYPTARCHIA_ENDPOINT: &str = "public_cryptarchia_endpoint";
const PUBLIC_CRYPTARCHIA_ENDPOINT_USERNAME: &str = "username";
const PUBLIC_CRYPTARCHIA_ENDPOINT_PASSWORD: &str = "password";

#[given(expr = "I have a cluster with capacity of {int} nodes")]
#[when(expr = "I have a cluster with capacity of {int} nodes")]
fn step_manual_cluster(world: &mut CucumberWorld, step: &Step, nodes_count: usize) -> StepResult {
    install_local_manual_cluster(
        world,
        ManualClusterSpec {
            kind: ManualClusterKind::Generated,
            capacity: nodes_count,
        },
    )
    .inspect_err(|e| {
        warn!(target: TARGET, "Step '{step}' error: {e}");
    })
}

#[given(expr = "I have a devnet cluster with capacity of {int} nodes")]
#[when(expr = "I have a devnet cluster with capacity of {int} nodes")]
fn step_manual_devnet_cluster(
    world: &mut CucumberWorld,
    step: &Step,
    nodes_count: usize,
) -> StepResult {
    install_local_manual_cluster(
        world,
        ManualClusterSpec {
            kind: ManualClusterKind::Devnet,
            capacity: nodes_count,
        },
    )
    .inspect_err(|e| {
        warn!(target: TARGET, "Step '{step}' error: {e}");
    })
}

#[given("the genesis block has the following wallet resources:")]
#[when("the genesis block has the following wallet resources:")]
fn step_cluster_has_wallet_resources(world: &mut CucumberWorld, step: &Step) -> StepResult {
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

#[given(expr = "we have a sponsored genesis fee account with {int} tokens of {int} value each")]
#[when(expr = "we have a sponsored genesis fee account with {int} tokens of {int} value each")]
fn step_sponsored_genesis_fee_account(
    world: &mut CucumberWorld,
    step: &Step,
    token_count: usize,
    token_value: u64,
) -> StepResult {
    ensure_fee_sponsorship_and_fork_groups_are_not_mixed(world, step.value.as_str())?;

    let token_count = non_zero!("genesis fee token count", token_count)?;
    let token_value = non_zero!("genesis fee token value", token_value)?;

    world
        .fee_state
        .set_sponsored_genesis_account(token_count, token_value);
    Ok(())
}

#[given("I start nodes with wallet resources:")]
#[when("I start nodes with wallet resources:")]
async fn step_start_nodes_with_wallet_resources(
    world: &mut CucumberWorld,
    step: &Step,
) -> StepResult {
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
    for (node_name, wallet_start_info, mut initial_peers) in nodes_to_start_ordered {
        initial_peers.sort();
        initial_peers.dedup();
        start_node(
            world,
            &step.value,
            &node_name,
            &wallet_start_info,
            &initial_peers,
        )
        .await?;
    }

    Ok(())
}

#[given(expr = "I start node {string}")]
#[when(expr = "I start node {string}")]
async fn step_start_manual_stand_alone_node(
    world: &mut CucumberWorld,
    step: &Step,
    node_name: String,
) -> StepResult {
    start_node(world, &step.value, &node_name, &Vec::new(), &Vec::new()).await
}

#[when(expr = "I connect node {string} to node {string} at runtime")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step entrypoints must take `&mut World`"
)]
async fn step_connect_nodes_at_runtime(
    world: &mut CucumberWorld,
    source_node_name: String,
    target_node_name: String,
) -> StepResult {
    connect_manual_node_to_node(world, &source_node_name, &target_node_name).await
}

#[then(expr = "node {string} has at least {int} peers within {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step entrypoints must take `&mut World`"
)]
async fn step_node_has_peers(
    world: &mut CucumberWorld,
    node_name: String,
    min_peers: usize,
    timeout_secs: u64,
) -> StepResult {
    assert_manual_node_has_peers(world, &node_name, min_peers, timeout_secs).await
}

#[when(expr = "I restart node {string}")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require the world as the first `&mut` argument"
)]
async fn step_restart_node(
    world: &mut CucumberWorld,
    step: &Step,
    node_name: String,
) -> StepResult {
    restart_node(world, &step.value, &node_name).await
}

#[given(expr = "we use IBD peers")]
#[when(expr = "we use IBD peers")]
const fn step_we_use_ibd_peers(world: &mut CucumberWorld) {
    world.populate_ibd_peers_from_initial_peers = Some(true);
}

#[given(expr = "we join an external network")]
#[when(expr = "we join an external network")]
const fn step_we_join_external_network(world: &mut CucumberWorld) {
    world.join_external_network = Some(true);
}

#[given(expr = "we will have distinct node groups to query wallet balances:")]
#[when(expr = "we will have distinct node groups to query wallet balances:")]
fn step_define_node_groups(world: &mut CucumberWorld, step: &Step) -> Result<(), StepError> {
    ensure_fee_sponsorship_and_fork_groups_are_not_mixed(world, &step.value)?;

    let table = step.table.as_ref().ok_or(StepError::LogicalError {
        message: "Expected a data table".to_owned(),
    })?;

    if table.rows.is_empty() || table.rows[0].len() != 2 {
        return Err(StepError::LogicalError {
            message: "Expected table columns: | group_name | node_name |".to_owned(),
        });
    }

    if table.rows[0][0].trim() != "group_name" || table.rows[0][1].trim() != "node_name" {
        return Err(StepError::LogicalError {
            message: "Expected table columns: | group_name | node_name |".to_owned(),
        });
    }

    world.node_groups.clear();
    world.node_to_group.clear();

    for row in table.rows.iter().skip(1) {
        if row.len() != 2 {
            return Err(StepError::LogicalError {
                message: "Each node-group row must have exactly two columns".to_owned(),
            });
        }

        let group_name = row[0].trim().to_owned();
        let node_name = row[1].trim().to_owned();

        if let Some(existing_group) = world.node_to_group.get(&node_name) {
            return Err(StepError::LogicalError {
                message: format!(
                    "Node `{node_name}` appears in both group `{existing_group}` and `{group_name}`"
                ),
            });
        }

        world
            .node_groups
            .entry(group_name.clone())
            .or_default()
            .insert(node_name.clone());
        world.node_to_group.insert(node_name, group_name);
    }

    Ok(())
}

#[given(expr = "I have user config setting {string} as {string}")]
#[when(expr = "I have user config setting {string} as {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Required by cucumber expression"
)]
fn step_set_user_config_setting(
    world: &mut CucumberWorld,
    step: &Step,
    setting_path: String,
    setting_value: String,
) -> StepResult {
    set_user_config_override(world, &step.value, &setting_path, &setting_value)
}

#[given(expr = "the first {int} nodes are declared as blend providers")]
#[when(expr = "the first {int} nodes are declared as blend providers")]
fn step_blend_provider_count(world: &mut CucumberWorld, provider_count: usize) -> StepResult {
    world.blend_core_nodes = Some(provider_count);
    rebuild_pending_local_manual_cluster(world)
}

#[given(expr = "no nodes are declared as blend providers")]
#[when(expr = "no nodes are declared as blend providers")]
fn step_no_blend_providers(world: &mut CucumberWorld) -> StepResult {
    world.blend_core_nodes = Some(0);
    rebuild_pending_local_manual_cluster(world)
}

#[given(expr = "I will create a blockchain snapshot {string} of all nodes when stopping")]
#[when(expr = "I will create a blockchain snapshot {string} of all nodes when stopping")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Required by cucumber expression"
)]
fn step_set_blockchain_snapshot_on_stop(
    world: &mut CucumberWorld,
    snapshot_name: String,
) -> StepResult {
    if snapshot_name.trim().is_empty() {
        return Err(StepError::InvalidArgument {
            message: "Snapshot name cannot be empty".to_owned(),
        });
    }
    validate_snapshot_path_component(&snapshot_name, "Snapshot name")?;
    world.blockchain_snapshot_name_on_stop = Some(snapshot_name.trim().to_owned());
    Ok(())
}

#[given(expr = "I will initialize started nodes from snapshot {string} source node {string}")]
#[when(expr = "I will initialize started nodes from snapshot {string} source node {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Required by cucumber expression"
)]
fn step_set_blockchain_snapshot_on_startup(
    world: &mut CucumberWorld,
    snapshot_name: String,
    node_name: String,
) -> StepResult {
    validate_snapshot_path_component(&snapshot_name, "Snapshot name")?;
    validate_snapshot_path_component(&node_name, "Node name")?;

    world.blockchain_snapshot_on_startup = Some(NodeSnapshot {
        name: snapshot_name.trim().to_owned(),
        node: node_name.trim().to_owned(),
    });
    Ok(())
}

#[given(expr = "I create a blockchain snapshot {string} of all nodes")]
#[when(expr = "I create a blockchain snapshot {string} of all nodes")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Required by cucumber expression"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require the world as the first `&mut` argument"
)]
fn step_create_blockchain_snapshot_all_nodes_now(
    world: &mut CucumberWorld,
    snapshot_name: String,
) -> StepResult {
    if world.nodes_info.is_empty() {
        return Err(StepError::InvalidArgument {
            message: "cannot create snapshot: no running nodes".to_owned(),
        });
    }

    create_snapshots_all_nodes(world, &snapshot_name)?;

    Ok(())
}

#[given(expr = "I create a blockchain snapshot {string} of node {string}")]
#[when(expr = "I create a blockchain snapshot {string} of node {string}")]
#[then(expr = "I create a blockchain snapshot {string} of node {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Required by cucumber expression"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require the world as the first `&mut` argument"
)]
fn step_create_blockchain_snapshot_node_now(
    world: &mut CucumberWorld,
    snapshot_name: String,
    node_name: String,
) -> StepResult {
    if world.nodes_info.is_empty() {
        return Err(StepError::InvalidArgument {
            message: "cannot create snapshot: no running nodes".to_owned(),
        });
    }

    if let Some(info) = world.nodes_info.get(&node_name) {
        save_named_blockchain_snapshot(&snapshot_name, &node_name, &info.runtime_dir)?;
        info!(
            target: TARGET,
            "Saved blockchain snapshot `{snapshot_name}` for node {}",
            info.runtime_dir.display()
        );
    } else {
        return Err(StepError::InvalidArgument {
            message: format!("Node {node_name} does not exist"),
        });
    }

    Ok(())
}

#[given("I have public cryptarchia endpoint peers:")]
#[when("I have public cryptarchia endpoint peers:")]
fn step_set_public_cryptarchia_endpoint_peers(
    world: &mut CucumberWorld,
    step: &Step,
) -> StepResult {
    let table = step.table.as_ref().ok_or(StepError::MissingTable)?;

    if table.rows.is_empty() {
        return Err(StepError::InvalidArgument {
            message: format!(
                "Step `{step}` error: public cryptarchia endpoint peers table cannot be empty"
            ),
        });
    }
    if table.rows.iter().any(|row| row.len() != 3) {
        return Err(StepError::InvalidArgument {
            message: format!(
                "Step `{step}` error: public cryptarchia endpoint peers table must have exactly three columns"
            ),
        });
    }
    if !matches!(table.rows[0][0].trim(), PUBLIC_CRYPTARCHIA_ENDPOINT)
        || !matches!(
            table.rows[0][1].trim(),
            PUBLIC_CRYPTARCHIA_ENDPOINT_USERNAME
        )
        || !matches!(
            table.rows[0][2].trim(),
            PUBLIC_CRYPTARCHIA_ENDPOINT_PASSWORD
        )
    {
        return Err(StepError::InvalidArgument {
            message: format!(
                "Step `{step}` error: public cryptarchia endpoint peers table header row must be \
                '{PUBLIC_CRYPTARCHIA_ENDPOINT}', '{PUBLIC_CRYPTARCHIA_ENDPOINT_USERNAME}', \
                '{PUBLIC_CRYPTARCHIA_ENDPOINT_PASSWORD}'"
            ),
        });
    }

    let mut endpoint_peers = Vec::with_capacity(table.rows.len().saturating_sub(1));
    for row in table.rows.iter().skip(1) {
        let url = parse_url(&row[0]).map_err(|e| StepError::InvalidArgument {
            message: format!(
                "Step `{}` error: invalid public cryptarchia endpoint '{}': {e}",
                step.value, row[0]
            ),
        })?;

        let username =
            resolve_literal_or_env(row[1].trim(), "public cryptarchia endpoint username").map_err(
                |e| StepError::InvalidArgument {
                    message: format!("Step `{}` error: {e}", step.value),
                },
            )?;
        if username.is_empty() {
            return Err(StepError::InvalidArgument {
                message: format!(
                    "Step `{}` error: username cannot be empty for public cryptarchia endpoint '{}'",
                    step.value, url
                ),
            });
        }

        let password =
            resolve_literal_or_env(row[2].trim(), "public cryptarchia endpoint password").map_err(
                |e| StepError::InvalidArgument {
                    message: format!("Step `{}` error: {e}", step.value),
                },
            )?;
        if password.is_empty() {
            return Err(StepError::InvalidArgument {
                message: format!(
                    "Step `{}` error: password cannot be empty for public cryptarchia endpoint '{}'",
                    step.value, url
                ),
            });
        }

        endpoint_peers.push(PublicCryptarchiaEndpointPeer {
            url,
            username,
            password,
        });
    }

    if endpoint_peers.is_empty() {
        return Err(StepError::InvalidArgument {
            message: format!(
                "Step `{step}` error: at least one public cryptarchia endpoint peer is required"
            ),
        });
    }
    world.public_cryptarchia_endpoint_peers = Some(endpoint_peers);

    Ok(())
}

#[given(expr = "all peers must be mode online after startup in {int} seconds")]
#[when(expr = "all peers must be mode online after startup in {int} seconds")]
const fn step_all_nodes_to_be_mode_online(world: &mut CucumberWorld, on_line_time_out: u64) {
    world.require_all_peers_mode_online_at_startup = Some(Duration::from_secs(on_line_time_out));
}

#[given("I have initial peers:")]
#[when("I have initial peers:")]
fn step_set_initial_peers(world: &mut CucumberWorld, step: &Step) -> StepResult {
    let table = step.table.as_ref().ok_or(StepError::MissingTable)?;
    if table.rows.is_empty() || table.rows[0].len() != 1 || table.rows[0][0] != "initial_peer" {
        return Err(StepError::InvalidArgument {
            message: format!(
                "Step `{}` error: initial peers table header must be `initial_peer`",
                step.value
            ),
        });
    }

    let mut peers = Vec::with_capacity(table.rows.len().saturating_sub(1));
    for row in table.rows.iter().skip(1) {
        let peer = row[0]
            .trim()
            .parse::<Multiaddr>()
            .map_err(|e| StepError::InvalidArgument {
                message: format!(
                    "Step `{}` error: invalid initial peer '{}': {e}",
                    step.value, row[0]
                ),
            })?;
        peers.push(peer);
    }

    world.initial_peers_override = Some(peers);
    Ok(())
}

#[given("I have IBD peers:")]
#[when("I have IBD peers:")]
fn step_set_ibd_peers(world: &mut CucumberWorld, step: &Step) -> StepResult {
    let table = step.table.as_ref().ok_or(StepError::MissingTable)?;
    if table.rows.is_empty() || table.rows[0].len() != 1 || table.rows[0][0] != "ibd_peer" {
        return Err(StepError::InvalidArgument {
            message: format!(
                "Step `{}` error: IBD peers table header must be `ibd_peer`",
                step.value
            ),
        });
    }

    let mut peers = std::collections::HashSet::with_capacity(table.rows.len().saturating_sub(1));
    for row in table.rows.iter().skip(1) {
        let peer = row[0]
            .trim()
            .parse::<PeerId>()
            .map_err(|e| StepError::InvalidArgument {
                message: format!(
                    "Step `{}` error: invalid IBD peer '{}': {e}",
                    step.value, row[0]
                ),
            })?;
        peers.insert(peer);
    }

    world.ibd_peers_override = Some(peers);
    world.populate_ibd_peers_from_initial_peers = Some(true);
    Ok(())
}

#[given(expr = "I start peer node {string} connected to node {string}")]
#[when(expr = "I start peer node {string} connected to node {string}")]
async fn step_start_manual_connected_node(
    world: &mut CucumberWorld,
    step: &Step,
    node_name: String,
    peer_name: String,
) -> StepResult {
    start_node(world, &step.value, &node_name, &Vec::new(), &[peer_name]).await
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
    start_node(
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
        poll_all_nodes_and_update_consensus_cache(&step.value, &mut world.nodes_info).await?;
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
    nodes_converged(world, &step.value, None, max_diff_height, time_out_seconds).await
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
    nodes_converged(
        world,
        &step.value,
        Some(min_height),
        max_diff_height,
        time_out_seconds,
    )
    .await
}

#[when(expr = "all nodes agree on LIB in {int} seconds")]
#[then(expr = "all nodes agree on LIB in {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require the world as the first `&mut` argument"
)]
async fn step_all_nodes_agree_on_lib(
    world: &mut CucumberWorld,
    step: &Step,
    time_out_seconds: u64,
) -> StepResult {
    ensure_all_nodes_agree_on_lib(world, &step.value, time_out_seconds).await
}

#[when("I wait for all nodes to be synced to the chain")]
#[then("I wait for all nodes to be synced to the chain")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require the world as the first `&mut` argument"
)]
async fn step_wait_for_all_nodes_to_be_synced_to_the_chain(
    world: &mut CucumberWorld,
    step: &Step,
) -> StepResult {
    wait_for_all_nodes_to_be_synced_to_chain(world, &step.value).await
}

#[when("I query cryptarchia info for all nodes")]
#[then("I query cryptarchia info for all nodes")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require the world as the first `&mut` argument"
)]
async fn step_query_cryptarchia_info_all_nodes(world: &mut CucumberWorld, step: &Step) {
    get_cryptarchia_info_all_nodes(world, &step.value).await;
}

#[then(expr = "I stop all nodes")]
fn step_stop_all_nodes(world: &mut CucumberWorld) -> StepResult {
    let runtime_dir_by_node_name: Vec<(String, String)> = world
        .nodes_info
        .iter()
        .map(|(node_name, info)| (node_name.clone(), info.started_node.name.clone()))
        .collect();

    stop_active_manual_cluster(world)?;

    if let Some(snapshot_name) = world.blockchain_snapshot_name_on_stop.as_ref() {
        create_snapshots_all_nodes(world, snapshot_name)?;
    }

    for (node_name, _) in &runtime_dir_by_node_name {
        info!(target: TARGET, "Stopping node '{node_name}'");
    }
    world.nodes_info.clear();

    Ok(())
}
