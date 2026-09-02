use super::{
    CucumberWorld, Duration, HashSet, Instant, Multiaddr, PUBLIC_CRYPTARCHIA_ENDPOINT,
    PUBLIC_CRYPTARCHIA_ENDPOINT_PASSWORD, PUBLIC_CRYPTARCHIA_ENDPOINT_USERNAME, PeerId,
    PublicCryptarchiaEndpointPeer, Step, StepError, StepResult, TARGET, given, info,
    nodes_converged, parse_url, poll_all_nodes_and_update_consensus_cache, resolve_literal_or_env,
    sleep, start_node, then, verify_reponsive_and_network_ready_with_timeout,
    wait_all_nodes_responive, when,
};

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
            base_url: url,
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
    world.startup.public_cryptarchia_endpoint_peers = Some(endpoint_peers);

    Ok(())
}

#[given(expr = "all peers must be mode online after startup in {int} seconds")]
#[when(expr = "all peers must be mode online after startup in {int} seconds")]
const fn step_all_nodes_to_be_mode_online(world: &mut CucumberWorld, on_line_time_out: u64) {
    world.startup.require_all_peers_mode_online_at_startup =
        Some(Duration::from_secs(on_line_time_out));
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

    world.startup.initial_peers_override = Some(peers);
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

    let mut peers = HashSet::with_capacity(table.rows.len().saturating_sub(1));
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

    world.startup.ibd_peers_override = Some(peers);
    world.startup.populate_ibd_peers_from_initial_peers = Some(true);
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
    start_node(
        world,
        &step.value,
        &node_name,
        &Vec::new(),
        &[peer_name],
        false,
        &[],
    )
    .await
}

#[given(expr = "I immediate start peer node {string} connected to node {string}")]
#[when(expr = "I immediate start peer node {string} connected to node {string}")]
async fn step_immediate_start_manual_connected_node(
    world: &mut CucumberWorld,
    step: &Step,
    node_name: String,
    peer_name: String,
) -> StepResult {
    start_node(
        world,
        &step.value,
        &node_name,
        &Vec::new(),
        &[peer_name],
        true,
        &[],
    )
    .await
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
        false,
        &[],
    )
    .await
}

#[given(expr = "I immediate start peer node {string} connected to node {string} and node {string}")]
#[when(expr = "I immediate start peer node {string} connected to node {string} and node {string}")]
async fn step_immediate_start_manual_two_connected_nodes(
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
        true,
        &[],
    )
    .await
}

#[when(expr = "I wait for all nodes to be responsive in {int} seconds")]
#[then(expr = "I wait for all nodes to be responsive in {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require the world as the first `&mut` argument"
)]
async fn step_wait_all_nodes_responsive(
    world: &mut CucumberWorld,
    step: &Step,
    time_out_seconds: u64,
) -> StepResult {
    let cluster = world
        .cluster
        .local_cluster
        .as_ref()
        .ok_or(StepError::LogicalError {
            message: "No local cluster available".into(),
        })?;
    if let Err(e) = wait_all_nodes_responive(cluster, Duration::from_secs(time_out_seconds)).await {
        return Err(StepError::StepFail {
            message: format!("Step `{}` error: {e}", step.value),
        });
    }

    let wait_tasks: Vec<_> = world
        .nodes_info
        .values()
        .map(|node| {
            let fut = verify_reponsive_and_network_ready_with_timeout(
                &node.started_node.client,
                &node.name,
                &node.started_node.name,
                Duration::from_secs(time_out_seconds),
            );
            let step_value = step.value.clone();
            async move {
                fut.await.map_err(|e| StepError::StepFail {
                    message: format!("Step `{step_value}` error: {e}"),
                })
            }
        })
        .collect();

    futures::future::try_join_all(wait_tasks).await?;

    Ok(())
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

#[when(expr = "I record node {string} height as {string}")]
async fn step_record_node_height(
    world: &mut CucumberWorld,
    node_name: String,
    height_alias: String,
) -> StepResult {
    let height = world
        .resolve_node_http_client(&node_name)?
        .consensus_info()
        .await?
        .cryptarchia_info
        .height;

    world
        .node_height_snapshots
        .record_height(height_alias.clone(), height)?;

    info!(
        target: TARGET,
        node = %node_name,
        alias = %height_alias,
        height,
        "Recorded node height"
    );

    Ok(())
}

#[then(
    expr = "node {string} reaches {int} blocks beyond recorded height {string} in {int} seconds"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step entrypoints must take `&mut World`"
)]
async fn step_node_reaches_blocks_beyond_recorded_height(
    world: &mut CucumberWorld,
    node_name: String,
    additional_blocks: u64,
    height_alias: String,
    timeout_seconds: u64,
) -> StepResult {
    let recorded_height = world.node_height_snapshots.height(&height_alias)?;
    let expected_height = recorded_height.saturating_add(additional_blocks);
    let client = world.resolve_node_http_client(&node_name)?;
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);

    loop {
        let height = client.consensus_info().await?.cryptarchia_info.height;
        if height >= expected_height {
            return Ok(());
        }

        if Instant::now() >= deadline {
            return Err(StepError::Timeout {
                message: format!(
                    "node '{node_name}' did not reach height {expected_height}; current height is {height}"
                ),
            });
        }

        sleep(Duration::from_millis(100)).await;
    }
}

#[when(expr = "node {string} is exactly at height")]
#[then(expr = "node {string} is exactly at height")]
async fn step_node_is_exactly_at_height(
    world: &mut CucumberWorld,
    step: &Step,
    node_name: String,
    height: u64,
) -> StepResult {
    poll_all_nodes_and_update_consensus_cache(&step.value, &mut world.nodes_info).await?;
    let node_height = world.node_best_height(&node_name)?.unwrap_or_default();
    if node_height != height {
        return Err(StepError::StepFail {
            message: format!(
                "Step `{}` error: Node '{node_name}' is at height {node_height}, required {height}",
                step.value
            ),
        });
    }
    Ok(())
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
