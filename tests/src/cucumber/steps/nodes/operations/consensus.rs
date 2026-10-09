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

fn active_node_names<'a>(
    names: impl Iterator<Item = &'a str>,
    stopped: &HashMap<String, Instant>,
) -> Vec<&'a str> {
    names.filter(|name| !stopped.contains_key(*name)).collect()
}

fn convergence_satisfied(
    status: &AlignmentStatus,
    height: u64,
    difference: u64,
    minimum: Option<u64>,
    maximum_difference: u64,
) -> bool {
    matches!(status, AlignmentStatus::Aligned)
        && difference <= maximum_difference
        && height >= minimum.unwrap_or_default()
}

async fn fetch_and_update_chain_info(
    step: &str,
    nodes_info: &mut HashMap<String, NodeInfo>,
    nodes_chain_info: &mut HashMap<String, ChainInfoMap>,
) -> Result<Option<(u64, u64, Vec<u64>)>, StepError> {
    // Evaluate convergence from this poll's tips, not cached tips from an
    // earlier height or a node that did not answer this poll.
    let mut current_nodes = nodes_info.clone();
    for node in current_nodes.values_mut() {
        node.chain_info.clear();
    }
    poll_all_nodes_and_update_consensus_cache(step, &mut current_nodes).await?;
    if current_nodes
        .values()
        .any(|node| node.best_height().is_none())
    {
        return Ok(None);
    }

    let mut best_node_heights: Vec<u64> = Vec::with_capacity(nodes_info.len());

    for node_info in current_nodes.values() {
        let max_height = node_info.best_height().unwrap_or_default();
        best_node_heights.push(max_height);
        if let Some(original) = nodes_info.get_mut(&node_info.name) {
            original
                .chain_info
                .retain(|height, _| *height <= max_height);
            for (height, hash) in &node_info.chain_info {
                original.upsert_tip(*height, hash.clone());
            }
        }

        let started_node_name = node_info.runtime_name.clone();
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

    Ok(Some((all_nodes_min, diff, best_node_heights)))
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
    #[cfg(feature = "security-audit")]
    crate::cucumber::steps::security_audit::verify_audit_participants_started(world)?;
    converge_nodes(
        &mut world.nodes_info,
        step,
        min_height,
        max_diff_height,
        time_out_seconds,
    )
    .await
}

pub async fn active_nodes_converged(
    world: &mut CucumberWorld,
    step: &str,
    max_diff_height: u64,
    time_out_seconds: u64,
) -> StepResult {
    #[cfg(feature = "security-audit")]
    crate::cucumber::steps::security_audit::verify_audit_participants_started(world)?;
    let selected = active_node_names(
        world.nodes_info.keys().map(String::as_str),
        &world.lifecycle.node_stopped_at,
    );
    let mut active_nodes = world
        .nodes_info
        .iter()
        .filter(|(name, _)| selected.contains(&name.as_str()))
        .map(|(name, node)| (name.clone(), node.clone()))
        .collect::<HashMap<_, _>>();
    if active_nodes.is_empty() {
        return Err(StepError::StepFail {
            message: format!("Step `{step}` error: no active nodes"),
        });
    }
    let result = converge_nodes(
        &mut active_nodes,
        step,
        None,
        max_diff_height,
        time_out_seconds,
    )
    .await;
    for (name, node) in active_nodes {
        if let Some(original) = world.nodes_info.get_mut(&name) {
            original.chain_info = node.chain_info;
        }
    }
    result
}

async fn converge_nodes(
    nodes_info: &mut HashMap<String, NodeInfo>,
    step: &str,
    min_height: Option<u64>,
    max_diff_height: u64,
    time_out_seconds: u64,
) -> StepResult {
    if nodes_info.is_empty() {
        return Err(StepError::StepFail {
            message: format!("Step `{step}` error: no nodes"),
        });
    }
    let start = Instant::now();
    let time_out = Duration::from_secs(time_out_seconds);

    // node_name -> (height -> header_id)  (overwrites on reorg)
    let mut nodes_chain_info: HashMap<String, ChainInfoMap> =
        HashMap::with_capacity(nodes_info.len());

    // Pre-initialize so lookups are deterministic
    for node_info in nodes_info.values() {
        nodes_chain_info
            .entry(node_info.runtime_name.clone())
            .or_default();
    }

    let mut count = 0usize;
    loop {
        let sample = timeout(
            time_out.saturating_sub(start.elapsed()),
            fetch_and_update_chain_info(step, nodes_info, &mut nodes_chain_info),
        )
        .await;
        if let Some((all_nodes_min, diff, peer_heights)) = match sample {
            Ok(result) => result?,
            Err(_) => None,
        } {
            let (status, anchor_hashes) =
                tips_aligned_at_min_difference(&nodes_chain_info, all_nodes_min);

            if start.elapsed() <= time_out
                && convergence_satisfied(&status, all_nodes_min, diff, min_height, max_diff_height)
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
            let consensus = node.client.consensus_info().await?;
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
        let result = node.client.consensus_info().await;
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
        if node_info.client.network_info().await.is_ok() {
            return Ok(());
        }
        sleep(Duration::from_millis(250)).await;
    }
    Err(format!(
        "Node `{node_name}` did not respond to network_info after {time_out_seconds:.2?}"
    ))
}

#[cfg(test)]
mod convergence_tests {
    use super::*;

    #[test]
    fn active_selection_excludes_only_explicitly_stopped_nodes() {
        let stopped = HashMap::from([("producer".to_owned(), Instant::now())]);
        assert_eq!(
            active_node_names(["producer", "alice", "bob"].into_iter(), &stopped),
            ["alice", "bob"]
        );
        assert_eq!(
            active_node_names(["producer"].into_iter(), &stopped),
            [] as [&str; 0]
        );
    }

    #[test]
    fn zero_skew_requires_equal_current_headers_and_minimum_height() {
        let mut chains = HashMap::from([
            ("alice".to_owned(), HashMap::from([(40, "same".to_owned())])),
            ("bob".to_owned(), HashMap::from([(40, "same".to_owned())])),
        ]);
        let (status, _) = tips_aligned_at_min_difference(&chains, 40);
        assert!(convergence_satisfied(&status, 40, 0, Some(40), 0));
        assert!(!convergence_satisfied(&status, 40, 1, Some(40), 0));
        assert!(!convergence_satisfied(&status, 40, 0, Some(41), 0));
        chains.get_mut("bob").unwrap().insert(40, "fork".to_owned());
        let (status, _) = tips_aligned_at_min_difference(&chains, 40);
        assert!(!convergence_satisfied(&status, 40, 0, Some(40), 0));
        chains.get_mut("bob").unwrap().clear();
        let (status, _) = tips_aligned_at_min_difference(&chains, 40);
        assert!(!convergence_satisfied(&status, 40, 0, Some(40), 0));
    }
}
