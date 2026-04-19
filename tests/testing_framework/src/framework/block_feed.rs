use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context as _, Result};
use common_http_client::ApiBlock;
use lb_node::HeaderId;
use testing_framework_core::scenario::{DynError, Feed, FeedRuntime, NodeClients};
use tokio::{
    sync::{RwLock, broadcast},
    time::sleep,
};
use tracing::{debug, warn};

use crate::node::NodeHttpClient;

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const CATCH_UP_WARN_AFTER: usize = 5;
const MAX_RETAINED_HEADERS_PER_PATH: usize = 128;
const SAFETY_WINDOW_BELOW_LIB: usize = 32;
const RETAIN_BELOW_MIN_LIB_HEIGHT: u64 = 1;

/// Broadcasts observed blocks to subscribers while tracking simple stats.
#[derive(Clone)]
pub struct BlockFeed {
    inner: Arc<BlockFeedInner>,
}

struct BlockFeedInner {
    sender: broadcast::Sender<Arc<BlockRecord>>,
    stats: Arc<BlockStats>,
    snapshot: RwLock<BlockFeedSnapshot>,
}

/// Node head view tracked by the feed.
#[derive(Clone, Debug)]
pub struct NodeHeadSnapshot {
    /// Stable node key (base URL string).
    pub node: String,
    /// Latest observed tip for this node.
    pub tip: HeaderId,
    /// Latest observed tip height for this node, if known to the feed.
    pub tip_height: Option<u64>,
    /// Latest observed LIB for this node.
    pub lib: HeaderId,
    /// Latest observed LIB height for this node, if known to the feed.
    pub lib_height: Option<u64>,
}

/// Read model exported by the feed for expectations.
#[derive(Clone, Debug, Default)]
pub struct BlockFeedSnapshot {
    /// Current head view for each tracked node.
    pub node_heads: Vec<NodeHeadSnapshot>,
    /// Known header -> parent edges accumulated during backfill.
    pub parent_edges: HashMap<HeaderId, HeaderId>,
    /// Cumulative number of pruned headers.
    pub pruned_blocks_total: u64,
}

/// Block payload observed in the latest feed cycle.
#[derive(Clone)]
pub struct ObservedBlock {
    /// Source node base URL.
    pub source_node: String,
    /// Block header identifier.
    pub header: HeaderId,
    /// Parent header identifier referenced by this block.
    pub parent: HeaderId,
    /// Full block body with transactions.
    pub block: Arc<ApiBlock>,
}

/// Fork-agnostic feed emission.
#[derive(Clone)]
pub struct BlockRecord {
    /// Newly discovered blocks since the previous cycle.
    pub new_blocks: Vec<ObservedBlock>,
    /// Latest per-node heads at emission time.
    pub node_heads: Vec<NodeHeadSnapshot>,
}

/// Background task driving the block feed.
pub struct BlockFeedRuntime {
    scanner: BlockScanner,
}

#[async_trait::async_trait]
impl FeedRuntime for BlockFeedRuntime {
    type Feed = BlockFeed;

    async fn run(self: Box<Self>) {
        let mut scanner = self.scanner;
        scanner.run().await;
    }
}

impl BlockFeed {
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<BlockRecord>> {
        self.inner.sender.subscribe()
    }

    #[must_use]
    pub fn stats(&self) -> Arc<BlockStats> {
        Arc::clone(&self.inner.stats)
    }

    /// Returns the latest feed snapshot used by expectations.
    pub async fn snapshot(&self) -> BlockFeedSnapshot {
        self.inner.snapshot.read().await.clone()
    }

    async fn update_snapshot(
        &self,
        node_heads: Vec<NodeHeadSnapshot>,
        parent_edges: HashMap<HeaderId, HeaderId>,
        pruned_blocks_total: u64,
    ) {
        let mut snapshot = self.inner.snapshot.write().await;
        snapshot.node_heads = node_heads;
        snapshot.parent_edges = parent_edges;
        snapshot.pruned_blocks_total = pruned_blocks_total;
    }

    fn ingest(&self, new_blocks: Vec<ObservedBlock>, node_heads: Vec<NodeHeadSnapshot>) {
        for observed in &new_blocks {
            self.inner.stats.record_block(observed.block.as_ref());
        }

        let record = Arc::new(BlockRecord {
            new_blocks,
            node_heads,
        });

        drop(self.inner.sender.send(record));
    }
}

impl Feed for BlockFeed {
    type Subscription = broadcast::Receiver<Arc<BlockRecord>>;

    fn subscribe(&self) -> Self::Subscription {
        self.inner.sender.subscribe()
    }
}

/// Prepare a block feed worker that polls blocks from all node clients and
/// broadcasts them.
pub async fn prepare_block_feed(
    node_clients: NodeClients<crate::framework::LbcEnv>,
) -> Result<(BlockFeed, BlockFeedRuntime), DynError> {
    let (sender, _) = broadcast::channel(1024);
    let feed = BlockFeed {
        inner: Arc::new(BlockFeedInner {
            sender,
            stats: Arc::new(BlockStats::default()),
            snapshot: RwLock::new(BlockFeedSnapshot::default()),
        }),
    };

    let mut scanner = BlockScanner::new(node_clients.snapshot(), feed.clone());
    scanner
        .catch_up()
        .await
        .map_err(|error| -> DynError { error.into() })?;

    Ok((feed, BlockFeedRuntime { scanner }))
}

struct BlockScanner {
    clients: Vec<NodeHttpClient>,
    feed: BlockFeed,
    seen: HashSet<HeaderId>,
    parents: HashMap<HeaderId, HeaderId>,
    heights: HashMap<HeaderId, u64>,
    node_heads: HashMap<String, NodeHeadSnapshot>,
    pruned_blocks_total: u64,
}

struct BackfillEntry {
    source_node: String,
    header: HeaderId,
    height: u64,
    block: ApiBlock,
}

impl BlockScanner {
    fn new(clients: Vec<NodeHttpClient>, feed: BlockFeed) -> Self {
        Self {
            clients,
            feed,
            seen: HashSet::new(),
            parents: HashMap::new(),
            heights: HashMap::new(),
            node_heads: HashMap::new(),
            pruned_blocks_total: 0,
        }
    }

    /// Runs the background polling loop and keeps block-feed state updated.
    async fn run(&mut self) {
        let mut consecutive_failures = 0usize;
        loop {
            if let Err(err) = self.catch_up().await {
                consecutive_failures += 1;
                log_catchup_failure(&err, consecutive_failures);
            } else if consecutive_failures > 0 {
                debug!(failures = consecutive_failures, "feed catch up recovered");

                consecutive_failures = 0;
            }

            sleep(POLL_INTERVAL).await;
        }
    }

    /// Polls all registered clients, backfills unseen headers, and emits one
    /// consolidated record.
    async fn catch_up(&mut self) -> Result<()> {
        let clients = self.clients.clone();
        let mut new_blocks = Vec::new();
        let mut processed = 0usize;

        for client in clients {
            let source_node = client.base_url().to_string();
            let info = match client.consensus_info().await {
                Ok(info) => info,
                Err(error) => {
                    debug!(source = %source_node, %error, "consensus_info failed; skipping source");
                    continue;
                }
            };

            self.node_heads.insert(
                source_node.clone(),
                NodeHeadSnapshot {
                    node: source_node.clone(),
                    tip: info.tip,
                    tip_height: Some(info.height),
                    lib: info.lib,
                    lib_height: self.heights.get(&info.lib).copied(),
                },
            );

            let backfill = self
                .collect_backfill(&client, &source_node, info.tip, info.height)
                .await?;

            let (count, mut discovered) = self.apply_backfill(backfill);
            processed += count;
            new_blocks.append(&mut discovered);
        }

        self.prune_graph();

        let heads = self.current_heads();
        self.feed
            .update_snapshot(
                heads.clone(),
                self.parents.clone(),
                self.pruned_blocks_total,
            )
            .await;

        if !new_blocks.is_empty() {
            self.feed.ingest(new_blocks, heads);
        }

        debug!(processed, "block feed processed catch up batch");

        Ok(())
    }

    /// Collects unseen blocks by walking parent links backwards from `tip`
    /// until known ancestry or a boundary condition is reached.
    async fn collect_backfill(
        &mut self,
        client: &NodeHttpClient,
        source_node: &str,
        tip: HeaderId,
        tip_height: u64,
    ) -> Result<Vec<BackfillEntry>> {
        let mut remaining_height = tip_height;
        let mut cursor_height = tip_height;
        let mut cursor = tip;
        let mut stack = Vec::new();

        loop {
            if self.seen.contains(&cursor) {
                break;
            }

            if remaining_height == 0 {
                self.seen.insert(cursor);
                self.heights.entry(cursor).or_insert(0);
                break;
            }

            let block = client
                .block(&cursor)
                .await?
                .context("missing block while catching up")?;
            let parent = block.header.parent_block;

            stack.push(BackfillEntry {
                source_node: source_node.to_owned(),
                header: cursor,
                height: cursor_height,
                block,
            });

            if self.seen.contains(&parent) || parent == cursor {
                break;
            }

            cursor = parent;
            remaining_height = remaining_height.saturating_sub(1);
            cursor_height = cursor_height.saturating_sub(1);
        }

        Ok(stack)
    }

    /// Applies collected backfill into scanner state and returns newly observed
    /// blocks.
    fn apply_backfill(&mut self, mut backfill: Vec<BackfillEntry>) -> (usize, Vec<ObservedBlock>) {
        let mut processed = 0usize;
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

            processed += 1;
        }

        (processed, new_blocks)
    }

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

    /// Prunes the in-memory block graph to a bounded retention set.
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

    /// Computes headers to keep.
    ///
    /// When all observed LIBs have known heights, retention is bounded by a
    /// consensus-aware boundary: keep headers with height >= `min_lib_height` -
    /// 1.
    ///
    /// When a LIB height is missing, falls back to bounded ancestry walks.
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

    fn min_known_lib_height(&self) -> Option<u64> {
        let heights = self
            .node_heads
            .values()
            .map(|head| self.heights.get(&head.lib).copied())
            .collect::<Option<Vec<_>>>()?;

        heights.into_iter().min()
    }

    fn retain_at_or_above_height(&self, boundary_height: u64) -> HashSet<HeaderId> {
        self.heights
            .iter()
            .filter_map(|(header, height)| (*height >= boundary_height).then_some(*header))
            .collect()
    }

    /// Collects a bounded ancestor set starting from `start`.
    fn path_set_to(&self, start: HeaderId) -> HashSet<HeaderId> {
        let mut out = HashSet::new();
        let mut cursor = start;
        let mut visited = HashSet::new();

        while out.len() < MAX_RETAINED_HEADERS_PER_PATH && visited.insert(cursor) {
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

    /// Keeps a bounded set of ancestors directly below LIB so near-LIB context
    /// survives pruning.
    fn lib_safety_window(&self, lib: HeaderId) -> HashSet<HeaderId> {
        let mut out = HashSet::new();
        let mut cursor = lib;
        let mut visited = HashSet::new();

        while out.len() < SAFETY_WINDOW_BELOW_LIB && visited.insert(cursor) {
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

/// Logs catch-up failures and escalates from debug to warning after repeated
/// consecutive failures.
fn log_catchup_failure(err: &anyhow::Error, consecutive_failures: usize) {
    if consecutive_failures >= CATCH_UP_WARN_AFTER {
        warn!(
            error = %err,
            error_debug = ?err,
            failures = consecutive_failures,
            "feed catch up failed repeatedly"
        );

        return;
    }

    debug!(
        error = %err,
        error_debug = ?err,
        failures = consecutive_failures,
        "feed catch up failed"
    );
}

/// Accumulates simple counters over observed blocks.
#[derive(Default)]
pub struct BlockStats {
    total_transactions: AtomicU64,
}

impl BlockStats {
    fn record_block(&self, block: &ApiBlock) {
        self.total_transactions
            .fetch_add(block.transactions.len() as u64, Ordering::Relaxed);
    }

    #[must_use]
    pub fn total_transactions(&self) -> u64 {
        self.total_transactions.load(Ordering::Relaxed)
    }
}
