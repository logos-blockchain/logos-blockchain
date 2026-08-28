use super::{
    CHAIN_SYNC_POLL_INTERVAL, CHAIN_SYNC_STATUS_LOG_INTERVAL, CRYPTARCHIA_INFO, ChainServiceInfo,
    Client, CryptarchiaInfo, CucumberWorld, Duration, HashMap, Instant, MajorityPublicSyncTarget,
    PublicCryptarchiaEndpointPeer, PublicPeerConsensusSnapshot, StepError, StepResult,
    SyncTargetStats, TARGET, Url, get_cryptarchia_info_all_nodes, info, sleep, truncate_hash, warn,
};

pub async fn wait_for_all_nodes_to_be_synced_to_chain(
    world: &mut CucumberWorld,
    step: &str,
) -> StepResult {
    let public_cryptarchia_endpoint_peers = world
        .startup
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

            catch_up_known_wallet_tracking_after_chain_sync(world, step).await?;

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

async fn catch_up_known_wallet_tracking_after_chain_sync(
    world: &mut CucumberWorld,
    step: &str,
) -> StepResult {
    if world.wallet_registry.wallet_info.is_empty() {
        return Ok(());
    }

    let started_at = Instant::now();
    // The wallet scanner timeout is moderate here because the contract is that the
    // majority nodes have been synced prior just to this step.
    world
        .wait_for_wallet_scanner_catch_up(Duration::from_secs(30))
        .await?;

    info!(
        target: TARGET,
        "Wallet scanner caught up after chain sync for step `{step}` in {:.2?}",
        started_at.elapsed()
    );

    Ok(())
}

pub fn parse_url(raw: &str) -> Result<String, String> {
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
                peer_url: peer.base_url.clone(),
                stats: SyncTargetStats::from_cryptarchia_info(&info),
            }),
            Err(e) => warn!(
                target: TARGET,
                "Failed to fetch public cryptarchia info from '{}': {e}",
                peer.base_url
            ),
        }
    }

    snapshots
}

/// Fetch the current consensus info from a public cryptarchia endpoint peer.
/// Returns an error if the request fails or the response is invalid.
pub async fn fetch_public_peer_consensus(
    client: &Client,
    peer: &PublicCryptarchiaEndpointPeer,
) -> Result<CryptarchiaInfo, StepError> {
    let request_url = Url::parse(&format!(
        "{peer_url}/{path}",
        peer_url = peer.base_url.as_str(),
        path = CRYPTARCHIA_INFO.trim_start_matches('/')
    ))
    .map_err(|e| StepError::InvalidArgument {
        message: format!(
            "Invalid public cryptarchia info URL for '{}': {e}",
            peer.base_url.as_str()
        ),
    })?;

    Ok(client
        .get(request_url)
        .basic_auth(&peer.username, Some(&peer.password))
        .send()
        .await?
        .error_for_status()?
        .json::<ChainServiceInfo>()
        .await
        .map_err(StepError::from)?
        .cryptarchia_info)
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
        if SyncTargetStats::from_cryptarchia_info(&consensus.cryptarchia_info) != target.stats {
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
