use std::{
    collections::{HashMap, HashSet},
    marker::PhantomData,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use lb_node::HeaderId;
use testing_framework_core::scenario::{DynError, Expectation, RunContext};
use thiserror::Error;

use crate::{framework::LbcEnv, workloads::LbcBlockFeedEnv};

/// Monitors a running cluster and fails the scenario as soon as nodes disagree
/// on LIB.
///
/// The monitor reads all node heads and parent edges from `BlockFeed`
/// snapshots and only fails on *proven* LIB conflicts (incompatible finalized
/// branches), not on transient tip divergence.
#[derive(Clone)]
pub struct ClusterForkMonitor<E = LbcEnv> {
    max_tip_set_size: usize,
    max_lib_set_size: usize,
    next_progress_log_at: Option<Instant>,
    ancestry_edges: HashMap<HeaderId, HeaderId>,
    _env: PhantomData<fn() -> E>,
}

#[derive(Debug, Error)]
enum ClusterForkMonitorError {
    #[error("cluster fork monitor requires at least 2 nodes")]
    InsufficientNodes,
    #[error("LIB divergence detected (max_lib_set={max_lib_set} max_tip_set={max_tip_set})")]
    LibDivergenceDetected {
        max_lib_set: usize,
        max_tip_set: usize,
    },
}

impl<E> Default for ClusterForkMonitor<E> {
    fn default() -> Self {
        Self {
            max_tip_set_size: 0,
            max_lib_set_size: 0,
            next_progress_log_at: None,
            ancestry_edges: HashMap::new(),
            _env: PhantomData,
        }
    }
}

const PROGRESS_LOG_INTERVAL: Duration = Duration::from_secs(60);

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

        self.next_progress_log_at = Some(Instant::now() + PROGRESS_LOG_INTERVAL);
        self.ancestry_edges.clear();

        Ok(())
    }

    async fn check_during_capture(&mut self, ctx: &RunContext<E>) -> Result<(), DynError> {
        let feed = E::block_feed(ctx);
        let snapshot = feed.snapshot().await;

        self.observe_snapshot(&snapshot.node_heads, &snapshot.parent_edges)?;
        self.maybe_log_node_heads(&snapshot.node_heads);

        Ok(())
    }

    async fn evaluate(&mut self, ctx: &RunContext<E>) -> Result<(), DynError> {
        let feed = E::block_feed(ctx);
        let snapshot = feed.snapshot().await;
        self.observe_snapshot(&snapshot.node_heads, &snapshot.parent_edges)?;
        let final_snapshot = format_node_heads(&snapshot.node_heads);

        tracing::info!(
            max_lib_set = self.max_lib_set_size,
            max_tip_set = self.max_tip_set_size,
            observed_blocks = self.ancestry_edges.len().max(snapshot.parent_edges.len()),
            final_snapshot = %final_snapshot.join(" | "),
            "cluster fork monitor summary"
        );

        Ok(())
    }
}

impl<E> ClusterForkMonitor<E> {
    fn maybe_log_node_heads(&mut self, node_heads: &[crate::NodeHeadSnapshot]) {
        let Some(next_log_at) = self.next_progress_log_at else {
            self.next_progress_log_at = Some(Instant::now() + PROGRESS_LOG_INTERVAL);
            return;
        };

        if Instant::now() < next_log_at {
            return;
        }

        self.next_progress_log_at = Some(Instant::now() + PROGRESS_LOG_INTERVAL);
        let samples = format_node_heads(node_heads);

        tracing::info!(
            nodes = node_heads.len(),
            snapshot = %samples.join(" | "),
            "cluster fork monitor progress"
        );
    }

    fn observe_snapshot(
        &mut self,
        node_heads: &[crate::NodeHeadSnapshot],
        parent_edges: &HashMap<HeaderId, HeaderId>,
    ) -> Result<(), DynError> {
        self.ancestry_edges
            .extend(parent_edges.iter().map(|(child, parent)| (*child, *parent)));

        let tip_set_size = node_heads
            .iter()
            .map(|head| head.tip)
            .collect::<HashSet<_>>()
            .len();

        let lib_set = node_heads
            .iter()
            .map(|head| head.lib)
            .collect::<HashSet<_>>();

        let lib_set_size = lib_set.len();

        self.max_tip_set_size = self.max_tip_set_size.max(tip_set_size);
        self.max_lib_set_size = self.max_lib_set_size.max(lib_set_size);

        let libs = lib_set.into_iter().collect::<Vec<_>>();
        if has_proven_lib_conflict(&libs, &self.ancestry_edges) {
            return Err(ClusterForkMonitorError::LibDivergenceDetected {
                max_lib_set: self.max_lib_set_size,
                max_tip_set: self.max_tip_set_size,
            }
            .into());
        }

        Ok(())
    }
}

fn format_node_heads(node_heads: &[crate::NodeHeadSnapshot]) -> Vec<String> {
    node_heads
        .iter()
        .map(|head| format!("node={} tip={:?} lib={:?}", head.node, head.tip, head.lib))
        .collect()
}

fn has_proven_lib_conflict(libs: &[HeaderId], parent_edges: &HashMap<HeaderId, HeaderId>) -> bool {
    for i in 0..libs.len() {
        for j in (i + 1)..libs.len() {
            if lib_pair_conflicts(libs[i], libs[j], parent_edges) {
                return true;
            }
        }
    }

    false
}

fn lib_pair_conflicts(
    left: HeaderId,
    right: HeaderId,
    parent_edges: &HashMap<HeaderId, HeaderId>,
) -> bool {
    if left == right {
        return false;
    }

    let left_on_right = reaches_with_completeness(right, left, parent_edges);
    if left_on_right.reaches {
        return false;
    }

    let right_on_left = reaches_with_completeness(left, right, parent_edges);
    if right_on_left.reaches {
        return false;
    }

    left_on_right.complete && right_on_left.complete
}

fn reaches_with_completeness(
    start: HeaderId,
    target: HeaderId,
    parent_edges: &HashMap<HeaderId, HeaderId>,
) -> Reachability {
    let mut cursor = start;
    let mut seen = HashSet::new();

    loop {
        if cursor == target {
            return Reachability {
                reaches: true,
                complete: true,
            };
        }

        if !seen.insert(cursor) {
            return Reachability {
                reaches: false,
                complete: false,
            };
        }

        let Some(parent) = parent_edges.get(&cursor).copied() else {
            return Reachability {
                reaches: false,
                complete: false,
            };
        };

        if parent == cursor {
            return Reachability {
                reaches: false,
                complete: true,
            };
        }

        cursor = parent;
    }
}

#[derive(Clone, Copy)]
struct Reachability {
    reaches: bool,
    complete: bool,
}
