use super::*;

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
                        "Started node '{started_node_name}' not found in chain info map",
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
                consensus.cryptarchia_info.height,
                consensus.cryptarchia_info.lib.encode_hex::<String>(),
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
    use futures_util::future::join_all;

    let nodes = nodes_info.values().collect::<Vec<&NodeInfo>>();

    // Query every node, but do not fail-fast on the first error.
    let info_futures = nodes.iter().map(async |node| {
        let node_name = node.name.clone();
        let result = node.started_node.client.consensus_info().await;
        (node_name, result)
    });

    let results = join_all(info_futures).await;

    let mut snapshots = Vec::<ConsensusSnapshot>::new();
    let mut failed_nodes = Vec::<String>::new();

    for (node_name, result) in results {
        match result {
            Ok(info) => snapshots.push(ConsensusSnapshot {
                node_name,
                height: info.cryptarchia_info.height,
                header_hash: info.cryptarchia_info.tip.encode_hex(),
            }),
            Err(e) => {
                // If both `consensus_info` and `network_info` fail, assume the node is no
                // longer responsive.
                if let Err(e2) = poll_network_info(
                    nodes_info.get_mut(&node_name).expect("Failed to get node"),
                    &node_name,
                    5,
                )
                .await
                {
                    return Err(StepError::StepFail {
                        message: format!(
                            "Step `{step}` error: {node_name} is not responsive anymore: {e} / {e2}"
                        ),
                    });
                }
                warn!(
                    target: TARGET,
                    "Step `{step}` error: node `{node_name}` did not respond with consensus_info: {e}",
                );
                failed_nodes.push(node_name);
            }
        }
    }

    // If all nodes failed in this poll, surface a hard error.
    // If at least one succeeded, update cache for those and let caller keep
    // polling.
    if snapshots.is_empty() {
        let failed = if failed_nodes.is_empty() {
            "none".to_owned()
        } else {
            failed_nodes.join(", ")
        };
        return Err(StepError::StepFail {
            message: format!(
                "Step `{step}` error: all nodes failed to respond with consensus_info in this poll \
                (failed: [{failed}])"
            ),
        });
    }

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

    if !failed_nodes.is_empty() {
        warn!(
            target: TARGET,
            "Step `{step}` warning: partial consensus poll failure; updated {}/{} node(s), failed: [{}]",
            snapshots.len(),
            snapshots.len() + failed_nodes.len(),
            failed_nodes.join(", "),
        );
    }

    Ok(())
}

async fn poll_network_info(
    node_info: &NodeInfo,
    node_name: &str,
    time_out_seconds: u64,
) -> Result<(), String> {
    let start = TokioInstant::now();
    let time_out = Duration::from_secs(time_out_seconds);
    while start.elapsed() <= time_out {
        if node_info.started_node.client.network_info().await.is_ok() {
            return Ok(());
        }
        sleep(Duration::from_millis(250)).await;
    }
    Err(format!(
        "Node `{node_name}` did not respond to network_info after {time_out_seconds:.2?}"
    ))
}
