use std::{
    collections::HashSet,
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::Result;
use lb_core::{block::Block, mantle::SignedMantleTx};
use lb_node::HeaderId;
use testing_framework_core::scenario::{DynError, Feed, FeedRuntime};
use tokio::{sync::broadcast, time::sleep};
use tracing::{debug, warn};

use crate::node::NodeHttpClient;

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const CATCH_UP_WARN_AFTER: usize = 5;
const MAX_CATCH_UP_BLOCKS_PER_POLL: usize = 64;

/// Broadcasts observed blocks to subscribers while tracking simple stats.
#[derive(Clone)]
pub struct BlockFeed {
    inner: Arc<BlockFeedInner>,
}

struct BlockFeedInner {
    sender: broadcast::Sender<Arc<BlockRecord>>,
    stats: Arc<BlockStats>,
}

/// Block header + payload snapshot emitted by the feed.
#[derive(Clone)]
pub struct BlockRecord {
    pub header: HeaderId,
    pub block: Arc<Block<SignedMantleTx>>,
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

    fn ingest(&self, header: HeaderId, block: Block<SignedMantleTx>) {
        self.inner.stats.record_block(&block);
        let record = Arc::new(BlockRecord {
            header,
            block: Arc::new(block),
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

/// Prepare a block feed worker that polls blocks from the given client and
/// broadcasts them.
pub async fn prepare_block_feed(
    client: NodeHttpClient,
) -> Result<(BlockFeed, BlockFeedRuntime), DynError> {
    let (sender, _) = broadcast::channel(1024);
    let feed = BlockFeed {
        inner: Arc::new(BlockFeedInner {
            sender,
            stats: Arc::new(BlockStats::default()),
        }),
    };

    let mut scanner = BlockScanner::new(client, feed.clone());
    scanner.catch_up().await.map_err(into_dyn_error)?;

    Ok((feed, BlockFeedRuntime { scanner }))
}

struct BlockScanner {
    client: NodeHttpClient,
    feed: BlockFeed,
    seen: HashSet<HeaderId>,
}

impl BlockScanner {
    fn new(client: NodeHttpClient, feed: BlockFeed) -> Self {
        Self {
            client,
            feed,
            seen: HashSet::new(),
        }
    }

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

    async fn catch_up(&mut self) -> Result<()> {
        let info = self.client.consensus_info().await?;

        let tip = info.tip;
        let mut remaining_height = info.height;

        let mut stack = Vec::new();
        let mut cursor = tip;
        let mut scanned = 0usize;

        loop {
            if self.seen.contains(&cursor) {
                break;
            }
            if scanned >= MAX_CATCH_UP_BLOCKS_PER_POLL {
                break;
            }

            if remaining_height == 0 {
                self.seen.insert(cursor);
                break;
            }

            let Some(block) = self.client.storage_block(&cursor).await? else {
                debug!(
                    tip = ?tip,
                    missing = ?cursor,
                    scanned,
                    "block feed catch up stopped early: missing historical block"
                );
                break;
            };

            let parent = block.header().parent();
            stack.push((cursor, block));

            if self.seen.contains(&parent) || parent == cursor {
                break;
            }

            cursor = parent;
            remaining_height = remaining_height.saturating_sub(1);
            scanned += 1;
        }

        let mut processed = 0usize;
        while let Some((header, block)) = stack.pop() {
            self.feed.ingest(header, block);
            self.seen.insert(header);

            processed += 1;
        }

        debug!(processed, "block feed processed catch up batch");

        Ok(())
    }
}

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

fn into_dyn_error(error: anyhow::Error) -> DynError {
    Box::<dyn Error + Send + Sync>::from(error)
}

/// Accumulates simple counters over observed blocks.
#[derive(Default)]
pub struct BlockStats {
    total_transactions: AtomicU64,
}

impl BlockStats {
    fn record_block(&self, block: &Block<SignedMantleTx>) {
        self.total_transactions
            .fetch_add(block.transactions().len() as u64, Ordering::Relaxed);
    }

    #[must_use]
    pub fn total_transactions(&self) -> u64 {
        self.total_transactions.load(Ordering::Relaxed)
    }
}
