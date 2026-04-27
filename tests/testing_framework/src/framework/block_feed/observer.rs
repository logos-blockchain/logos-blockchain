use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use common_http_client::ApiBlock;
use lb_node::HeaderId;
use testing_framework_core::observation::{
    ObservationBatch, ObservationConfig, ObservedSource, Observer,
};
use tracing::debug;

use crate::node::NodeHttpClient;

const BLOCK_FEED_INTERVAL: Duration = Duration::from_secs(1);
const BLOCK_FEED_HISTORY_LIMIT: usize = 1024;
const MAX_RETAINED_HEADERS_PER_PATH: usize = 128;
const SAFETY_WINDOW_BELOW_LIB: usize = 32;
const RETAIN_BELOW_MIN_LIB_HEIGHT: u64 = 1;

/// Observer configuration and logic for Logos chain state.
#[derive(Clone, Debug, Default)]
pub struct BlockFeedObserver;

impl BlockFeedObserver {
    /// Default runtime configuration for the Logos block observer.
    #[must_use]
    pub const fn config() -> ObservationConfig {
        ObservationConfig {
            interval: BLOCK_FEED_INTERVAL,
            history_limit: BLOCK_FEED_HISTORY_LIMIT,
        }
    }
}

/// Node head view tracked by the observer.
#[derive(Clone, Debug)]
pub struct NodeHeadSnapshot {
    /// Stable node key used by the source provider.
    pub node: String,
    /// Latest observed tip for this node.
    pub tip: HeaderId,
    /// Latest observed slot for this node.
    pub slot: u64,
    /// Latest observed tip height for this node, if known.
    pub tip_height: Option<u64>,
    /// Latest observed LIB for this node.
    pub lib: HeaderId,
    /// Latest observed LIB height for this node, if known.
    pub lib_height: Option<u64>,
}

/// Read model exported to expectations and reports.
#[derive(Clone, Debug, Default)]
pub struct BlockFeedSnapshot {
    /// Current head view for each tracked node.
    pub node_heads: Vec<NodeHeadSnapshot>,
    /// Known header -> parent edges accumulated during backfill.
    pub parent_edges: HashMap<HeaderId, HeaderId>,
    /// Known header -> height data accumulated during backfill.
    pub header_heights: HashMap<HeaderId, u64>,
    /// Cumulative number of pruned headers.
    pub pruned_blocks_total: u64,
}

impl BlockFeedSnapshot {
    /// Returns one observed node head by name.
    #[must_use]
    pub fn node_head(&self, node_name: &str) -> Option<&NodeHeadSnapshot> {
        self.node_heads
            .iter()
            .find(|node_head| node_head.node == node_name)
    }

    /// Returns the retained height for one observed header, if available.
    #[must_use]
    pub fn header_height(&self, header: &HeaderId) -> Option<u64> {
        self.header_heights.get(header).copied()
    }

    /// Returns a concise summary for timeout and validation errors.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.node_heads.is_empty() {
            return "no node heads observed".to_owned();
        }

        self.node_heads
            .iter()
            .map(|node_head| {
                format!(
                    "node={} slot={} height={:?} tip={:?} lib={:?}",
                    node_head.node,
                    node_head.slot,
                    node_head.tip_height,
                    node_head.tip,
                    node_head.lib
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// One block observed from one source during one observation cycle.
#[derive(Clone)]
pub struct ObservedBlock {
    /// Source node key.
    pub source_node: String,
    /// Block header identifier.
    pub header: HeaderId,
    /// Parent header identifier referenced by this block.
    pub parent: HeaderId,
    /// Full block body with transactions.
    pub block: Arc<ApiBlock>,
}

/// One non-empty batch of newly observed blocks.
pub type BlockRecord = ObservationBatch<ObservedBlock>;

/// Latest consensus view fetched from one source before backfill begins.
///
/// Keeping this as a small struct makes the refresh flow read in phases:
/// fetch current head, record it, then backfill any unseen ancestry.
struct SourceHead {
    tip: HeaderId,
    tip_height: u64,
    snapshot: NodeHeadSnapshot,
}

/// One block discovered while walking backward from a source tip.
///
/// Entries are collected from tip to ancestor, then applied in reverse so the
/// retained graph is updated in parent-before-child order.
struct BackfillEntry {
    source_node: String,
    header: HeaderId,
    height: u64,
    block: ApiBlock,
}

#[async_trait]
impl Observer for BlockFeedObserver {
    type Source = NodeHttpClient;
    type State = BlockFeedState;
    type Snapshot = BlockFeedSnapshot;
    type Event = ObservedBlock;

    async fn init(
        &self,
        sources: &[ObservedSource<Self::Source>],
    ) -> Result<Self::State, Box<dyn std::error::Error + Send + Sync>> {
        let mut state = BlockFeedState::default();
        let new_blocks = state.refresh(sources).await?;

        debug!(
            source_count = sources.len(),
            new_blocks = new_blocks.len(),
            "initialized block feed observer"
        );

        Ok(state)
    }

    async fn poll(
        &self,
        sources: &[ObservedSource<Self::Source>],
        state: &mut Self::State,
    ) -> Result<Vec<Self::Event>, Box<dyn std::error::Error + Send + Sync>> {
        state.refresh(sources).await.map_err(Into::into)
    }

    fn snapshot(&self, state: &Self::State) -> Self::Snapshot {
        state.snapshot()
    }
}

/// Internal retained state for `BlockFeedObserver`.
#[derive(Default)]
pub struct BlockFeedState {
    seen: HashSet<HeaderId>,
    parents: HashMap<HeaderId, HeaderId>,
    heights: HashMap<HeaderId, u64>,
    node_heads: HashMap<String, NodeHeadSnapshot>,
    pruned_blocks_total: u64,
}

impl BlockFeedState {
    /// Refreshes every observed source and updates the retained ancestry graph.
    ///
    /// The observer keeps only a bounded window around the currently visible
    /// tips and LIBs, so pruning always happens after one full refresh pass.
    async fn refresh(
        &mut self,
        sources: &[ObservedSource<NodeHttpClient>],
    ) -> Result<Vec<ObservedBlock>> {
        let mut new_blocks = Vec::new();
        let mut processed = 0usize;

        for source in sources {
            let discovered = self.refresh_source(source).await?;
            processed += discovered.len();
            new_blocks.extend(discovered);
        }

        self.prune_graph();

        debug!(
            processed,
            sources = sources.len(),
            new_blocks = new_blocks.len(),
            "block feed observer refreshed state"
        );

        Ok(new_blocks)
    }

    /// Refreshes one source from its current consensus head down to the first
    /// already-known ancestor.
    async fn refresh_source(
        &mut self,
        source: &ObservedSource<NodeHttpClient>,
    ) -> Result<Vec<ObservedBlock>> {
        let Some(head) = self.fetch_source_head(source).await? else {
            return Ok(Vec::new());
        };

        self.record_node_head(head.snapshot);

        let backfill = self
            .collect_backfill(&source.source, &source.name, head.tip, head.tip_height)
            .await?;

        Ok(self.apply_backfill(backfill))
    }

    /// Fetches the current tip/LIB view for one source.
    ///
    /// Consensus-info failures are treated as a skipped source instead of a
    /// whole-observer failure so one unhealthy node does not stop the feed.
    async fn fetch_source_head(
        &self,
        source: &ObservedSource<NodeHttpClient>,
    ) -> Result<Option<SourceHead>> {
        let info = match source.source.consensus_info().await {
            Ok(info) => info,
            Err(error) => {
                debug!(
                    source = %source.name,
                    error = %error,
                    "consensus_info failed; skipping source"
                );

                return Ok(None);
            }
        };

        Ok(Some(SourceHead {
            tip: info.tip,
            tip_height: info.height,
            snapshot: NodeHeadSnapshot {
                node: source.name.clone(),
                tip: info.tip,
                slot: info.slot.into_inner(),
                tip_height: Some(info.height),
                lib: info.lib,
                lib_height: self.heights.get(&info.lib).copied(),
            },
        }))
    }

    /// Stores the latest visible head for one source.
    fn record_node_head(&mut self, snapshot: NodeHeadSnapshot) {
        self.node_heads.insert(snapshot.node.clone(), snapshot);
    }

    /// Walks backward from one tip until the retained graph catches up.
    ///
    /// The walk stops as soon as it reaches an already-seen header, a
    /// self-parent, or height zero.
    async fn collect_backfill(
        &mut self,
        client: &NodeHttpClient,
        source_name: &str,
        tip: HeaderId,
        tip_height: u64,
    ) -> Result<Vec<BackfillEntry>> {
        let mut cursor_height = tip_height;
        let mut cursor = tip;
        let mut stack = Vec::new();

        loop {
            if self.seen.contains(&cursor) {
                break;
            }

            if cursor_height == 0 {
                self.record_known_header(cursor, 0);
                break;
            }

            let block = client
                .block(&cursor)
                .await?
                .context("missing block while catching up")?;
            let parent = block.header.parent_block;

            stack.push(BackfillEntry {
                source_node: source_name.to_owned(),
                header: cursor,
                height: cursor_height,
                block,
            });

            if self.seen.contains(&parent) || parent == cursor {
                break;
            }

            cursor = parent;
            cursor_height = cursor_height.saturating_sub(1);
        }

        Ok(stack)
    }

    /// Applies a tip-to-ancestor backfill stack in ancestry order.
    fn apply_backfill(&mut self, mut backfill: Vec<BackfillEntry>) -> Vec<ObservedBlock> {
        let mut new_blocks = Vec::new();

        while let Some(BackfillEntry {
            source_node,
            header,
            height,
            block,
        }) = backfill.pop()
        {
            let parent = block.header.parent_block;
            self.parents.insert(header, parent);
            self.seen.insert(header);
            self.heights.insert(header, height);
            self.heights
                .entry(parent)
                .or_insert_with(|| height.saturating_sub(1));

            new_blocks.push(ObservedBlock {
                source_node,
                header,
                parent,
                block: Arc::new(block),
            });
        }

        new_blocks
    }

    /// Records a header whose height is already known without fetching a body.
    fn record_known_header(&mut self, header: HeaderId, height: u64) {
        self.seen.insert(header);
        self.heights.entry(header).or_insert(height);
    }

    /// Materializes the current read-side snapshot exposed to workloads.
    fn snapshot(&self) -> BlockFeedSnapshot {
        BlockFeedSnapshot {
            node_heads: self.current_heads(),
            parent_edges: self.parents.clone(),
            header_heights: self.heights.clone(),
            pruned_blocks_total: self.pruned_blocks_total,
        }
    }

    /// Returns node heads with tip/LIB heights refreshed from retained state.
    fn current_heads(&self) -> Vec<NodeHeadSnapshot> {
        let mut heads = self
            .node_heads
            .values()
            .cloned()
            .map(|mut head| {
                head.tip_height = self.heights.get(&head.tip).copied().or(head.tip_height);
                head.lib_height = self.heights.get(&head.lib).copied();
                head
            })
            .collect::<Vec<_>>();
        heads.sort_by(|left, right| left.node.cmp(&right.node));
        heads
    }

    /// Drops retained headers outside the current tip/LIB safety window.
    fn prune_graph(&mut self) {
        let retain = self.retained_headers();
        if retain.is_empty() {
            return;
        }

        let mut removed = 0usize;
        self.seen.retain(|header| {
            let keep = retain.contains(header);
            if !keep {
                removed += 1;
            }

            keep
        });

        self.parents.retain(|header, _| retain.contains(header));
        self.heights.retain(|header, _| retain.contains(header));
        self.pruned_blocks_total = self.pruned_blocks_total.saturating_add(removed as u64);
    }

    /// Computes the set of headers that must remain queryable after pruning.
    ///
    /// When every node has a known LIB height we retain everything at or above
    /// the minimum LIB boundary. Before that point we fall back to bounded tip
    /// and LIB ancestry windows so early snapshots still remain navigable.
    fn retained_headers(&self) -> HashSet<HeaderId> {
        let min_lib_height = self.min_known_lib_height();
        let mut retain = HashSet::new();

        for head in self.node_heads.values() {
            retain.insert(head.tip);
            retain.insert(head.lib);
        }

        if let Some(min_lib_height) = min_lib_height {
            let boundary_height = min_lib_height.saturating_sub(RETAIN_BELOW_MIN_LIB_HEIGHT);
            retain.extend(self.retain_at_or_above_height(boundary_height));

            return retain;
        }

        for head in self.node_heads.values() {
            retain.extend(self.path_set_to(head.tip));
            retain.extend(self.lib_safety_window(head.lib));
        }

        retain
    }

    /// Returns the minimum retained LIB height once every node LIB is known.
    fn min_known_lib_height(&self) -> Option<u64> {
        let heights = self
            .node_heads
            .values()
            .map(|head| self.heights.get(&head.lib).copied())
            .collect::<Option<Vec<_>>>()?;

        heights.into_iter().min()
    }

    /// Retains every known header at or above one height boundary.
    fn retain_at_or_above_height(&self, boundary_height: u64) -> HashSet<HeaderId> {
        self.heights
            .iter()
            .filter_map(|(header, height)| (*height >= boundary_height).then_some(*header))
            .collect()
    }

    /// Returns a bounded ancestry walk from one tip toward genesis.
    fn path_set_to(&self, start: HeaderId) -> HashSet<HeaderId> {
        self.bounded_path_set(start, MAX_RETAINED_HEADERS_PER_PATH)
    }

    /// Returns the small ancestry window kept below one observed LIB.
    fn lib_safety_window(&self, lib: HeaderId) -> HashSet<HeaderId> {
        self.bounded_path_set(lib, SAFETY_WINDOW_BELOW_LIB)
    }

    /// Walks parent links backward until the limit, a repeat, or a root.
    fn bounded_path_set(&self, start: HeaderId, limit: usize) -> HashSet<HeaderId> {
        let mut out = HashSet::new();
        let mut cursor = start;
        let mut visited = HashSet::new();

        while out.len() < limit && visited.insert(cursor) {
            out.insert(cursor);

            let Some(parent) = self.parents.get(&cursor).copied() else {
                break;
            };

            if parent == cursor {
                break;
            }

            cursor = parent;
        }

        out
    }
}
