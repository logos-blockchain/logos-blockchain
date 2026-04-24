mod report;

use std::{
    collections::{HashMap, HashSet},
    marker::PhantomData,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use lb_node::{HeaderId, config::deployment::DeploymentSettings};
use report::{ReportFormatter, SummaryState};
use testing_framework_core::scenario::{DynError, Expectation, RunContext};
use thiserror::Error;

use crate::{
    NodeHeadSnapshot,
    framework::LbcEnv,
    node::configs::{default_e2e_deployment_settings, deployment::TopologyConfig},
    workloads::LbcBlockFeedEnv,
};

const DEFAULT_TIP_STALL_THRESHOLD: Duration = Duration::from_mins(3);
const DEFAULT_NODE_TIP_STALL_THRESHOLD: Duration = Duration::from_mins(6);
const DEFAULT_LIB_STALL_THRESHOLD: Duration = Duration::from_mins(6);
const PROGRESS_LOG_INTERVAL: Duration = Duration::from_mins(1);
const BROADCAST_LATENCY: Duration = Duration::from_secs(1);

/// Monitors a running cluster and fails the scenario as soon as nodes disagree
/// on LIB.
///
/// The monitor reads all node heads and parent edges from `BlockFeed`
/// snapshots and only fails on *proven* LIB conflicts (incompatible finalized
/// branches), not on transient tip divergence.
#[derive(Clone)]
pub struct ClusterForkMonitor<E = LbcEnv> {
    /// Largest number of distinct tips observed in any snapshot.
    max_tip_set_size: usize,
    /// Largest number of distinct LIBs observed in any snapshot.
    max_lib_set_size: usize,
    /// Longest time a single node stayed behind the cluster tip.
    longest_node_tip_stall: Duration,
    /// Largest tip-height spread observed inside one snapshot.
    worst_tip_gap: u64,
    /// Largest LIB-height spread observed inside one snapshot.
    worst_lib_gap: u64,
    /// Cryptarchia security parameter used as the finalized-history window.
    security_param: u64,
    /// Cluster-wide progress tracker for the best observed tip.
    tip_progress: ClusterProgressState,
    /// Per-node stall budget used when one node stops following the cluster.
    node_tip_stall_threshold: Duration,
    /// Cluster-wide progress tracker for the best observed LIB.
    lib_progress: ClusterProgressState,
    /// Next timestamp when a periodic progress log should be emitted.
    next_progress_log_at: Option<Instant>,
    /// Union of all observed child -> parent links from feed snapshots.
    ancestry_edges: HashMap<HeaderId, HeaderId>,
    /// Per-node tip progress state used to detect lagging nodes.
    node_progress: HashMap<String, NodeProgressState>,
    /// Binds the monitor to the scenario environment without storing a value.
    _env: PhantomData<fn() -> E>,
}

#[derive(Debug, Error)]
enum ClusterForkMonitorError {
    #[error("cluster fork monitor requires at least 2 nodes")]
    InsufficientNodes,
    #[error(
        "LIB divergence detected (max_lib_set={max_lib_set} max_tip_set={max_tip_set}) details={details}"
    )]
    LibDivergenceDetected {
        max_lib_set: usize,
        max_tip_set: usize,
        details: String,
    },
    #[error(
        "cluster tip stalled for {stalled_for:?} at height {max_tip_height}; threshold={threshold:?}; snapshot={snapshot}"
    )]
    TipProgressStalled {
        stalled_for: Duration,
        threshold: Duration,
        max_tip_height: u64,
        snapshot: String,
    },
    #[error(
        "cluster LIB stalled for {stalled_for:?} at height {max_lib_height}; threshold={threshold:?}; snapshot={snapshot}"
    )]
    LibProgressStalled {
        stalled_for: Duration,
        threshold: Duration,
        max_lib_height: u64,
        snapshot: String,
    },
    #[error(
        "node tip stalled for {stalled_for:?} node={node} node_tip_height={node_tip_height} cluster_tip_height={cluster_tip_height}; threshold={threshold:?}; snapshot={snapshot}"
    )]
    NodeTipProgressStalled {
        node: String,
        stalled_for: Duration,
        threshold: Duration,
        node_tip_height: u64,
        cluster_tip_height: u64,
        snapshot: String,
    },
}

/// Derived view of one feed snapshot, assembled once and reused by checks.
struct SnapshotAnalysis<'a> {
    /// Per-node heads returned by the block feed.
    node_heads: &'a [NodeHeadSnapshot],
    /// Parent links returned by the block feed for ancestry reconstruction.
    parent_edges: &'a HashMap<HeaderId, HeaderId>,
    /// Number of distinct tips present in the snapshot.
    tip_set_size: usize,
    /// Number of distinct LIBs present in the snapshot.
    lib_set_size: usize,
    /// Highest tip height observed in the snapshot.
    max_tip_height: Option<u64>,
    /// Highest LIB height observed in the snapshot.
    max_lib_height: Option<u64>,
    /// Difference between highest and lowest tip heights in the snapshot.
    tip_height_gap: u64,
    /// Difference between highest and lowest LIB heights in the snapshot.
    lib_height_gap: u64,
    /// Distinct LIB headers present in the snapshot.
    lib_headers: Vec<HeaderId>,
    /// Preformatted node-head summary reused in errors and logs.
    snapshot_description: String,
}

impl<'a> SnapshotAnalysis<'a> {
    /// Builds a reusable analysis object from one block-feed snapshot.
    fn new(
        node_heads: &'a [NodeHeadSnapshot],
        parent_edges: &'a HashMap<HeaderId, HeaderId>,
    ) -> Self {
        let max_tip_height = node_heads.iter().filter_map(|head| head.tip_height).max();
        let min_tip_height = node_heads.iter().filter_map(|head| head.tip_height).min();
        let max_lib_height = node_heads.iter().filter_map(|head| head.lib_height).max();
        let min_lib_height = node_heads.iter().filter_map(|head| head.lib_height).min();
        let lib_headers = distinct_lib_headers(node_heads);

        Self {
            node_heads,
            parent_edges,
            tip_set_size: distinct_tip_count(node_heads),
            lib_set_size: lib_headers.len(),
            max_tip_height,
            max_lib_height,
            tip_height_gap: height_gap(max_tip_height, min_tip_height),
            lib_height_gap: height_gap(max_lib_height, min_lib_height),
            lib_headers,
            snapshot_description: ReportFormatter::snapshot_description_for(node_heads),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HeaderAtHeight {
    header: HeaderId,
    height: u64,
}

/// Shared progress-tracking logic for cluster-wide tip/LIB movement.
#[derive(Clone, Copy)]
struct ClusterProgressState {
    /// Best height observed so far.
    max_height: Option<u64>,
    /// Longest no-progress interval seen so far.
    longest_stall: Duration,
    /// Timestamp when progress last advanced.
    last_progress_at: Option<Instant>,
    /// Threshold after which a stall becomes a failure.
    threshold: Duration,
}

impl ClusterProgressState {
    /// Creates an empty progress tracker with the given failure threshold.
    const fn new(threshold: Duration) -> Self {
        Self {
            max_height: None,
            longest_stall: Duration::ZERO,
            last_progress_at: None,
            threshold,
        }
    }

    /// Clears all observed progress while preserving the configured threshold.
    const fn reset(&mut self) {
        self.max_height = None;
        self.longest_stall = Duration::ZERO;
        self.last_progress_at = None;
    }

    /// Replaces the current failure threshold.
    const fn set_threshold(&mut self, threshold: Duration) {
        self.threshold = threshold;
    }

    /// Records an observed height and returns the current stall duration if no
    /// progress happened.
    fn observe(&mut self, observed_height: u64, now: Instant) -> Option<Duration> {
        match self.max_height {
            Some(previous) if observed_height > previous => {
                self.max_height = Some(observed_height);
                self.last_progress_at = Some(now);

                None
            }
            Some(_) => {
                let stalled_for = now.duration_since(
                    self.last_progress_at
                        .expect("progress timestamp must exist when max height is known"),
                );
                self.longest_stall = self.longest_stall.max(stalled_for);

                Some(stalled_for)
            }
            None => {
                self.max_height = Some(observed_height);
                self.last_progress_at = Some(now);

                None
            }
        }
    }
}

/// Per-node progress state used to detect nodes that stop following the tip.
#[derive(Clone)]
struct NodeProgressState {
    /// Highest tip height observed for this node.
    max_tip_height: u64,
    /// Timestamp when this node last advanced its tip.
    last_progress_at: Instant,
}

impl NodeProgressState {
    /// Starts tracking a node from its first observed tip height.
    const fn new(initial_tip_height: u64, observed_at: Instant) -> Self {
        Self {
            max_tip_height: initial_tip_height,
            last_progress_at: observed_at,
        }
    }

    /// Updates the node state after a tip advance.
    const fn record_progress(&mut self, tip_height: u64, observed_at: Instant) {
        self.max_tip_height = tip_height;
        self.last_progress_at = observed_at;
    }
}

impl<E> Default for ClusterForkMonitor<E> {
    fn default() -> Self {
        Self {
            max_tip_set_size: 0,
            max_lib_set_size: 0,
            longest_node_tip_stall: Duration::ZERO,
            worst_tip_gap: 0,
            worst_lib_gap: 0,
            security_param: 0,
            tip_progress: ClusterProgressState::new(DEFAULT_TIP_STALL_THRESHOLD),
            node_tip_stall_threshold: DEFAULT_NODE_TIP_STALL_THRESHOLD,
            lib_progress: ClusterProgressState::new(DEFAULT_LIB_STALL_THRESHOLD),
            next_progress_log_at: None,
            ancestry_edges: HashMap::new(),
            node_progress: HashMap::new(),
            _env: PhantomData,
        }
    }
}

/// Derives runtime stall budgets from the same deployment path used by the
/// local testing environment.
fn derive_thresholds<E>(ctx: &RunContext<E>) -> (Duration, Duration, Duration)
where
    E: LbcBlockFeedEnv,
{
    let config: &TopologyConfig = ctx.descriptors().config();
    let Some(genesis_tx) = config.genesis_tx.clone() else {
        return (
            DEFAULT_TIP_STALL_THRESHOLD,
            DEFAULT_NODE_TIP_STALL_THRESHOLD,
            DEFAULT_LIB_STALL_THRESHOLD,
        );
    };

    let deployment = default_e2e_deployment_settings(genesis_tx);
    let node_count = ctx.node_clients().len() as u64;

    (
        propagation_budget(3, node_count, &deployment, 3.0),
        propagation_budget(6, node_count, &deployment, 3.0),
        propagation_budget(
            deployment.cryptarchia.security_param.get(),
            node_count,
            &deployment,
            3.0,
        ),
    )
}

/// Computes a conservative propagation budget for `num_blocks` worth of
/// progress under the current deployment timing.
fn propagation_budget(
    num_blocks: u32,
    blend_network_size: u64,
    deployment: &DeploymentSettings,
    margin_factor: f64,
) -> Duration {
    let proposal_interval = deployment
        .time
        .slot_duration
        .div_f64(deployment.cryptarchia.slot_activation_coeff.as_f64());

    let blend_latency =
        if blend_network_size < deployment.blend.common.minimum_network_size.into_inner() {
            Duration::ZERO
        } else {
            deployment.blend_round_duration().saturating_mul(
                (deployment
                    .blend
                    .core
                    .scheduler
                    .delayer
                    .maximum_release_delay_in_rounds
                    .get()
                    * deployment.blend.common.num_blend_layers.get())
                .try_into()
                .expect("blend latency multiplier must fit u32"),
            )
        };

    proposal_interval
        .saturating_add(blend_latency)
        .saturating_add(BROADCAST_LATENCY)
        .saturating_mul(num_blocks)
        .mul_f64(margin_factor)
}

#[async_trait]
impl<E> Expectation<E> for ClusterForkMonitor<E>
where
    E: LbcBlockFeedEnv,
{
    fn name(&self) -> &'static str {
        "cluster_fork_monitor"
    }

    async fn start_capture(&mut self, ctx: &RunContext<E>) -> Result<(), DynError> {
        if ctx.node_clients().len() < 2 {
            return Err(ClusterForkMonitorError::InsufficientNodes.into());
        }

        self.reset_capture_state();
        self.apply_thresholds(derive_thresholds(ctx));
        self.security_param = u64::from(ctx.descriptors().config().security_param);

        Ok(())
    }

    async fn check_during_capture(&mut self, ctx: &RunContext<E>) -> Result<(), DynError> {
        let snapshot = E::block_feed(ctx)?.snapshot();
        let analysis = SnapshotAnalysis::new(&snapshot.node_heads, &snapshot.parent_edges);

        self.observe_snapshot(&analysis)?;
        self.maybe_log_progress(&analysis);

        Ok(())
    }

    async fn evaluate(&mut self, ctx: &RunContext<E>) -> Result<(), DynError> {
        let snapshot = E::block_feed(ctx)?.snapshot();
        let analysis = SnapshotAnalysis::new(&snapshot.node_heads, &snapshot.parent_edges);

        self.observe_snapshot(&analysis)?;

        let summary = ReportFormatter::summary(
            &snapshot,
            &self.ancestry_edges,
            SummaryState {
                max_lib_set: self.max_lib_set_size,
                max_tip_set: self.max_tip_set_size,
                max_tip_height: self.tip_progress.max_height,
                max_lib_height: self.lib_progress.max_height,
                longest_tip_stall: self.tip_progress.longest_stall,
                longest_node_tip_stall: self.longest_node_tip_stall,
                longest_lib_stall: self.lib_progress.longest_stall,
                tip_stall_threshold: self.tip_progress.threshold,
                node_tip_stall_threshold: self.node_tip_stall_threshold,
                lib_stall_threshold: self.lib_progress.threshold,
                worst_tip_gap: self.worst_tip_gap,
                worst_lib_gap: self.worst_lib_gap,
            },
        );

        tracing::info!(?summary, "cluster fork monitor summary");

        Ok(())
    }
}

impl<E> ClusterForkMonitor<E> {
    /// Resets all runtime state at the start of a new capture window.
    fn reset_capture_state(&mut self) {
        self.next_progress_log_at = Some(Instant::now() + PROGRESS_LOG_INTERVAL);
        self.max_tip_set_size = 0;
        self.max_lib_set_size = 0;
        self.longest_node_tip_stall = Duration::ZERO;
        self.worst_tip_gap = 0;
        self.worst_lib_gap = 0;
        self.tip_progress.reset();
        self.lib_progress.reset();
        self.ancestry_edges.clear();
        self.node_progress.clear();
    }

    /// Applies thresholds derived from the current deployment settings.
    const fn apply_thresholds(
        &mut self,
        (tip_stall_threshold, node_tip_stall_threshold, lib_stall_threshold): (
            Duration,
            Duration,
            Duration,
        ),
    ) {
        self.tip_progress.set_threshold(tip_stall_threshold);
        self.node_tip_stall_threshold = node_tip_stall_threshold;
        self.lib_progress.set_threshold(lib_stall_threshold);
    }

    /// Emits a periodic progress snapshot when the log interval elapses.
    fn maybe_log_progress(&mut self, analysis: &SnapshotAnalysis<'_>) {
        let Some(next_log_at) = self.next_progress_log_at else {
            self.next_progress_log_at = Some(Instant::now() + PROGRESS_LOG_INTERVAL);

            return;
        };

        if Instant::now() < next_log_at {
            return;
        }

        self.next_progress_log_at = Some(Instant::now() + PROGRESS_LOG_INTERVAL);

        let progress = ReportFormatter::progress_snapshot(
            analysis.node_heads,
            analysis,
            self.tip_progress.threshold,
            self.lib_progress.threshold,
        );

        tracing::info!(?progress, "cluster fork monitor progress");
    }

    /// Ingests one analyzed snapshot into ancestry, health, and divergence
    /// checks.
    fn observe_snapshot(&mut self, analysis: &SnapshotAnalysis<'_>) -> Result<(), DynError> {
        self.record_ancestry_edges(analysis.parent_edges);
        self.record_snapshot_shape(analysis);
        self.observe_runtime_health(analysis)?;
        self.ensure_no_lib_divergence(analysis)?;

        Ok(())
    }

    /// Merges newly observed parent links into the global ancestry graph.
    fn record_ancestry_edges(&mut self, parent_edges: &HashMap<HeaderId, HeaderId>) {
        self.ancestry_edges
            .extend(parent_edges.iter().map(|(child, parent)| (*child, *parent)));
    }

    /// Updates max set sizes and worst spread metrics for the current snapshot.
    fn record_snapshot_shape(&mut self, analysis: &SnapshotAnalysis<'_>) {
        self.max_tip_set_size = self.max_tip_set_size.max(analysis.tip_set_size);
        self.max_lib_set_size = self.max_lib_set_size.max(analysis.lib_set_size);
        self.worst_tip_gap = self.worst_tip_gap.max(analysis.tip_height_gap);
        self.worst_lib_gap = self.worst_lib_gap.max(analysis.lib_height_gap);
    }

    /// Runs all runtime health checks that do not require ancestry reasoning.
    fn observe_runtime_health(&mut self, analysis: &SnapshotAnalysis<'_>) -> Result<(), DynError> {
        let now = Instant::now();

        if let Some(max_tip_height) = analysis.max_tip_height {
            self.observe_cluster_tip_progress(max_tip_height, now, analysis)?;
            self.observe_node_tip_progress(max_tip_height, now, analysis)?;
        }

        if let Some(max_lib_height) = analysis.max_lib_height {
            self.observe_cluster_lib_progress(max_lib_height, now, analysis)?;
        }

        Ok(())
    }

    /// Updates the cluster tip progress tracker and fails on sustained stalls.
    fn observe_cluster_tip_progress(
        &mut self,
        max_tip_height: u64,
        now: Instant,
        analysis: &SnapshotAnalysis<'_>,
    ) -> Result<(), DynError> {
        if let Some(stalled_for) = self.tip_progress.observe(max_tip_height, now)
            && stalled_for > self.tip_progress.threshold
        {
            return Err(ClusterForkMonitorError::TipProgressStalled {
                stalled_for,
                threshold: self.tip_progress.threshold,
                max_tip_height,
                snapshot: analysis.snapshot_description.clone(),
            }
            .into());
        }

        Ok(())
    }

    /// Updates per-node tip progress and fails when one node lags behind the
    /// moving cluster tip for too long.
    fn observe_node_tip_progress(
        &mut self,
        cluster_tip_height: u64,
        now: Instant,
        analysis: &SnapshotAnalysis<'_>,
    ) -> Result<(), DynError> {
        for head in analysis.node_heads {
            let Some(node_tip_height) = head.tip_height else {
                continue;
            };

            let state = self
                .node_progress
                .entry(head.node.clone())
                .or_insert_with(|| NodeProgressState::new(node_tip_height, now));

            if node_tip_height > state.max_tip_height {
                state.record_progress(node_tip_height, now);

                continue;
            }

            if cluster_tip_height <= state.max_tip_height {
                continue;
            }

            let stalled_for = now.duration_since(state.last_progress_at);
            self.longest_node_tip_stall = self.longest_node_tip_stall.max(stalled_for);

            if stalled_for > self.node_tip_stall_threshold {
                return Err(ClusterForkMonitorError::NodeTipProgressStalled {
                    node: head.node.clone(),
                    stalled_for,
                    threshold: self.node_tip_stall_threshold,
                    node_tip_height: state.max_tip_height,
                    cluster_tip_height,
                    snapshot: analysis.snapshot_description.clone(),
                }
                .into());
            }
        }

        Ok(())
    }

    /// Updates the cluster LIB progress tracker and fails on sustained stalls.
    fn observe_cluster_lib_progress(
        &mut self,
        max_lib_height: u64,
        now: Instant,
        analysis: &SnapshotAnalysis<'_>,
    ) -> Result<(), DynError> {
        if let Some(stalled_for) = self.lib_progress.observe(max_lib_height, now)
            && stalled_for > self.lib_progress.threshold
        {
            return Err(ClusterForkMonitorError::LibProgressStalled {
                stalled_for,
                threshold: self.lib_progress.threshold,
                max_lib_height,
                snapshot: analysis.snapshot_description.clone(),
            }
            .into());
        }

        Ok(())
    }

    /// Fails when distinct LIBs are proven incompatible by the ancestry graph.
    fn ensure_no_lib_divergence(&self, analysis: &SnapshotAnalysis<'_>) -> Result<(), DynError> {
        if has_proven_lib_conflict(
            analysis.node_heads,
            &self.ancestry_edges,
            self.security_param,
        ) {
            let details = ReportFormatter::new(analysis.node_heads, &self.ancestry_edges)
                .lib_divergence_details(&analysis.lib_headers);

            return Err(ClusterForkMonitorError::LibDivergenceDetected {
                max_lib_set: self.max_lib_set_size,
                max_tip_set: self.max_tip_set_size,
                details,
            }
            .into());
        }

        Ok(())
    }
}

/// Counts distinct tip headers in a snapshot.
fn distinct_tip_count(node_heads: &[NodeHeadSnapshot]) -> usize {
    node_heads
        .iter()
        .map(|head| head.tip)
        .collect::<HashSet<_>>()
        .len()
}

/// Collects distinct LIB headers in a snapshot.
fn distinct_lib_headers(node_heads: &[NodeHeadSnapshot]) -> Vec<HeaderId> {
    node_heads
        .iter()
        .map(|head| head.lib)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect()
}

/// Computes the distance between the highest and lowest observed heights.
const fn height_gap(max_height: Option<u64>, min_height: Option<u64>) -> u64 {
    match (max_height, min_height) {
        (Some(max_height), Some(min_height)) => max_height.saturating_sub(min_height),
        _ => 0,
    }
}

/// Returns true only when two LIBs are proven incompatible.
fn has_proven_lib_conflict(
    node_heads: &[NodeHeadSnapshot],
    parent_edges: &HashMap<HeaderId, HeaderId>,
    security_param: u64,
) -> bool {
    let libs = distinct_libs_with_heights(node_heads);
    for i in 0..libs.len() {
        for j in (i + 1)..libs.len() {
            if lib_pair_conflicts(libs[i], libs[j], parent_edges, security_param) {
                return true;
            }
        }
    }

    false
}

/// Collects one representative height for each distinct LIB header.
fn distinct_libs_with_heights(node_heads: &[NodeHeadSnapshot]) -> Vec<HeaderAtHeight> {
    let mut libs = HashMap::new();
    for head in node_heads {
        let Some(height) = head.lib_height else {
            continue;
        };
        libs.entry(head.lib).or_insert(height);
    }

    libs.into_iter()
        .map(|(header, height)| HeaderAtHeight { header, height })
        .collect()
}

/// Determines whether one LIB pair is provably forked.
fn lib_pair_conflicts(
    left: HeaderAtHeight,
    right: HeaderAtHeight,
    parent_edges: &HashMap<HeaderId, HeaderId>,
    security_param: u64,
) -> bool {
    if left.header == right.header {
        return false;
    }

    if left.height == right.height {
        return left.height >= security_param;
    }

    let (higher, lower) = if left.height > right.height {
        (left, right)
    } else {
        (right, left)
    };

    if higher.height < security_param || lower.height < security_param {
        return false;
    }

    let depth = higher.height.saturating_sub(lower.height);
    walk_ancestor_by_depth(higher.header, depth, parent_edges)
        .is_some_and(|ancestor| ancestor != lower.header)
}

fn walk_ancestor_by_depth(
    start: HeaderId,
    depth: u64,
    parent_edges: &HashMap<HeaderId, HeaderId>,
) -> Option<HeaderId> {
    let mut cursor = start;
    let mut seen = HashSet::new();

    for _ in 0..depth {
        if !seen.insert(cursor) {
            return None;
        }

        let parent = parent_edges.get(&cursor).copied()?;
        if parent == cursor {
            return None;
        }

        cursor = parent;
    }

    Some(cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(byte: u8) -> HeaderId {
        HeaderId::from([byte; 32])
    }

    #[test]
    fn lib_conflict_same_height_after_k() {
        let left = HeaderAtHeight {
            header: h(1),
            height: 25,
        };
        let right = HeaderAtHeight {
            header: h(2),
            height: 25,
        };

        assert!(lib_pair_conflicts(left, right, &HashMap::new(), 20));
    }

    #[test]
    fn lib_conflict_same_height_before_k_is_ignored() {
        let left = HeaderAtHeight {
            header: h(1),
            height: 5,
        };
        let right = HeaderAtHeight {
            header: h(2),
            height: 5,
        };

        assert!(!lib_pair_conflicts(left, right, &HashMap::new(), 20));
    }

    #[test]
    fn lib_conflict_when_higher_branch_does_not_reach_lower() {
        let mut parents = HashMap::new();
        parents.insert(h(3), h(4));
        parents.insert(h(4), h(5));
        parents.insert(h(5), h(6));

        let higher = HeaderAtHeight {
            header: h(3),
            height: 30,
        };
        let lower = HeaderAtHeight {
            header: h(9),
            height: 27,
        };

        assert!(lib_pair_conflicts(higher, lower, &parents, 20));
    }

    #[test]
    fn lib_conflict_not_reported_when_higher_reaches_lower() {
        let mut parents = HashMap::new();
        parents.insert(h(3), h(4));
        parents.insert(h(4), h(5));
        parents.insert(h(5), h(9));

        let higher = HeaderAtHeight {
            header: h(3),
            height: 30,
        };
        let lower = HeaderAtHeight {
            header: h(9),
            height: 27,
        };

        assert!(!lib_pair_conflicts(higher, lower, &parents, 20));
    }
}
