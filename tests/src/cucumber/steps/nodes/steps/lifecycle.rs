use super::{
    ConfigOverride, CucumberWorld, Duration, GenesisTokens, HashMap, Instant, ManualClusterKind,
    ManualClusterSpec, NodesToStartUnordered, Step, StepError, StepResult, TARGET, WalletAccount,
    assert_manual_node_has_peers, connect_manual_node_to_node,
    ensure_fee_sponsorship_and_fork_groups_are_not_mixed, given, install_local_manual_cluster,
    non_zero, parse_genesis_wallet_tokens_row, parse_mining_wallet_resources_table_row,
    parse_wallet_resources_table_row, restart_node, start_node,
    start_nodes_order_respecting_dependencies, stop_node, then,
    verify_genesis_wallet_resources_table_indexes,
    verify_mining_node_wallet_resources_table_indexes, verify_node_wallet_resources_table_indexes,
    warn, when,
};

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
    world.chain.genesis_tokens.clear();
    for row in table.rows.iter().skip(1) {
        let (account_index, token_count, token_amount) =
            parse_genesis_wallet_tokens_row(&step.value, row)?;

        world.chain.genesis_tokens.push(GenesisTokens {
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
        .wallet_registry
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

    let nodes_to_start_ordered = start_nodes_order_respecting_dependencies(
        nodes_to_start,
        world.nodes_info.keys().cloned().collect(),
    )
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
            false,
            &[],
        )
        .await?;
    }

    world.ensure_wallet_scanner_started().await?;

    Ok(())
}

/// Starts mining nodes, each carrying one or more wallet resources of which
/// exactly one is flagged `is_mining_wallet`. That wallet's public key is
/// derived and applied as the node's `pow.claim_address`, so mined rewards are
/// paid to a wallet the test tracks. Multiple mining nodes are supported; each
/// gets its own single mining wallet / claim address.
#[given("I start mining nodes with wallet resources:")]
#[when("I start mining nodes with wallet resources:")]
async fn step_start_mining_nodes_with_wallet_resources(
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

    verify_mining_node_wallet_resources_table_indexes(table, &step.value)?;

    let mut nodes_to_start: NodesToStartUnordered = HashMap::new();
    let mut node_claim_overrides: HashMap<String, ConfigOverride> = HashMap::new();
    let mut node_mining_wallet_count: HashMap<String, usize> = HashMap::new();
    for row in table.rows.iter().skip(1) {
        let (node_name, wallet_start_info, is_mining_wallet, connected_to) =
            parse_mining_wallet_resources_table_row(&step.value, row)?;

        if is_mining_wallet {
            *node_mining_wallet_count
                .entry(node_name.clone())
                .or_insert(0) += 1;
            let account =
                WalletAccount::deterministic(wallet_start_info.account_index as u64, 0, true)
                    .map_err(|source| StepError::InvalidArgument {
                        message: format!(
                            "Step `{}` error: failed to derive mining wallet public key for \
                            `{}`: {source}",
                            step.value, wallet_start_info.wallet_name
                        ),
                    })?;
            let value = serde_yaml::to_value(account.public_key()).map_err(|source| {
                StepError::InvalidArgument {
                    message: format!(
                        "Step `{}` error: failed to serialize mining wallet public key for \
                            `{}`: {source}",
                        step.value, wallet_start_info.wallet_name
                    ),
                }
            })?;
            node_claim_overrides.insert(
                node_name.clone(),
                ConfigOverride {
                    path: "pow.claim_address".to_owned(),
                    value,
                },
            );
        }

        let entry = nodes_to_start
            .entry(node_name)
            .or_insert_with(|| (Vec::new(), Vec::new()));
        entry.0.push(wallet_start_info);
        if let Some(peer) = connected_to {
            entry.1.push(peer);
        }
    }

    // Every mining node must configure exactly one mining wallet.
    for node_name in nodes_to_start.keys() {
        let count = node_mining_wallet_count
            .get(node_name)
            .copied()
            .unwrap_or(0);
        if count != 1 {
            return Err(StepError::InvalidArgument {
                message: format!(
                    "Step `{}` error: mining node `{node_name}` must have exactly one \
                    is_mining_wallet row, found {count}",
                    step.value
                ),
            });
        }
    }

    let nodes_to_start_ordered = start_nodes_order_respecting_dependencies(
        nodes_to_start,
        world.nodes_info.keys().cloned().collect(),
    )
    .inspect_err(|e| {
        warn!(target: TARGET, "Step `{}` error: {e}", step.value);
    })?;
    for (node_name, wallet_start_info, mut initial_peers) in nodes_to_start_ordered {
        initial_peers.sort();
        initial_peers.dedup();
        let extra_user_overrides = node_claim_overrides
            .get(&node_name)
            .map_or(&[][..], std::slice::from_ref);
        start_node(
            world,
            &step.value,
            &node_name,
            &wallet_start_info,
            &initial_peers,
            false,
            extra_user_overrides,
        )
        .await?;
    }

    world.ensure_wallet_scanner_started().await?;

    Ok(())
}

#[given(expr = "I start node {string}")]
#[when(expr = "I start node {string}")]
async fn step_start_manual_stand_alone_node(
    world: &mut CucumberWorld,
    step: &Step,
    node_name: String,
) -> StepResult {
    start_node(
        world,
        &step.value,
        &node_name,
        &Vec::new(),
        &Vec::new(),
        false,
        &[],
    )
    .await
}

#[given(expr = "I immediate start node {string}")]
#[when(expr = "I immediate start node {string}")]
async fn step_start_manual_network_ready_only_stand_alone_node(
    world: &mut CucumberWorld,
    step: &Step,
    node_name: String,
) -> StepResult {
    start_node(
        world,
        &step.value,
        &node_name,
        &Vec::new(),
        &Vec::new(),
        true,
        &[],
    )
    .await
}

#[when(expr = "I start node {string} to be ready between {int} and {int} seconds")]
async fn step_start_manual_stand_alone_node_not_ready_before(
    world: &mut CucumberWorld,
    step: &Step,
    node_name: String,
    min_wait_seconds: u64,
    max_wait_seconds: u64,
) -> StepResult {
    let start = Instant::now();
    start_node(
        world,
        &step.value,
        &node_name,
        &Vec::new(),
        &Vec::new(),
        false,
        &[],
    )
    .await?;

    let elapsed = start.elapsed();
    if elapsed < Duration::from_secs(min_wait_seconds) {
        return Err(StepError::StepFail {
            message: format!(
                "Step `{}` error: Node '{node_name}' became ready too early: elapsed {:.2?}, \
                expected at least {min_wait_seconds}s",
                step.value, elapsed,
            ),
        });
    }
    if elapsed > Duration::from_secs(max_wait_seconds) {
        return Err(StepError::StepFail {
            message: format!(
                "Step `{}` error: Node '{node_name}' took too long to become ready: elapsed {:.2?}, \
                expected at most {max_wait_seconds}s",
                step.value, elapsed,
            ),
        });
    }

    Ok(())
}

#[when(
    expr = "I start peer node {string} connected to node {string} to be ready between {int} and {int} seconds"
)]
async fn step_start_manual_peer_node_not_ready_before(
    world: &mut CucumberWorld,
    step: &Step,
    node_name: String,
    peer_name: String,
    min_wait_seconds: u64,
    max_wait_seconds: u64,
) -> StepResult {
    let start = Instant::now();
    start_node(
        world,
        &step.value,
        &node_name,
        &Vec::new(),
        &[peer_name],
        false,
        &[],
    )
    .await?;

    let elapsed = start.elapsed();
    if elapsed < Duration::from_secs(min_wait_seconds) {
        return Err(StepError::StepFail {
            message: format!(
                "Step `{}` error: Node '{node_name}' became ready too early: elapsed {:.2?}, \
                expected at least {min_wait_seconds}s",
                step.value, elapsed,
            ),
        });
    }
    if elapsed > Duration::from_secs(max_wait_seconds) {
        return Err(StepError::StepFail {
            message: format!(
                "Step `{}` error: Node '{node_name}' took too long to become ready: elapsed {:.2?}, \
                expected at most {max_wait_seconds}s",
                step.value, elapsed,
            ),
        });
    }

    Ok(())
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
async fn step_restart_node(
    world: &mut CucumberWorld,
    step: &Step,
    node_name: String,
) -> StepResult {
    restart_node(world, &step.value, &node_name).await?;
    if world.blend_diagnostics.observation_count > 0 {
        world.blend_diagnostics.stopped_nodes.remove(&node_name);
        world.blend_diagnostics.phase =
            Some(crate::cucumber::world::BlendDiagnosticPhase::Recovery);
    }
    Ok(())
}

#[when(expr = "I stop node {string}")]
async fn step_stop_node(world: &mut CucumberWorld, step: &Step, node_name: String) -> StepResult {
    if world.blend_diagnostics.observation_count > 0 {
        world.blend_diagnostics.phase = Some(crate::cucumber::world::BlendDiagnosticPhase::Outage);
    }
    stop_node(world, &step.value, &node_name).await?;
    if world.blend_diagnostics.observation_count > 0 {
        world.blend_diagnostics.stopped_nodes.insert(node_name);
    }
    Ok(())
}
