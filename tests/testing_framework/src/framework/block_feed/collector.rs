use std::{collections::BTreeMap, time::Duration};

use testing_framework_core::scenario::DynError;
use tokio::task::JoinHandle;
use tracing::warn;

use super::BlockFeed;

const COLLECTOR_IDLE_WAIT: Duration = Duration::from_secs(1);

/// Consumer hook for projecting higher-level state from the shared block feed.
///
/// Collectors should be idempotent. The runtime calls each collector whenever
/// the feed advances and also after short idle waits, so delayed observation
/// errors can still be surfaced by consumers.
pub trait BlockFeedCollector: Send + 'static {
    /// Stable name used in collector diagnostics.
    fn name(&self) -> &'static str;

    /// Projects the latest feed state into consumer-specific state.
    fn collect(&mut self, feed: &BlockFeed) -> Result<(), DynError>;
}

pub type BoxedBlockFeedCollector = Box<dyn BlockFeedCollector>;

/// Runs block-feed collectors against one shared feed observation stream.
pub struct BlockFeedCollectorRuntime {
    task: JoinHandle<()>,
}

impl BlockFeedCollectorRuntime {
    #[must_use]
    pub fn start(feed: BlockFeed, collectors: Vec<BoxedBlockFeedCollector>) -> Self {
        Self {
            task: tokio::spawn(run_collectors(feed, collectors)),
        }
    }
}

impl Drop for BlockFeedCollectorRuntime {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn run_collectors(feed: BlockFeed, mut collectors: Vec<BoxedBlockFeedCollector>) {
    if collectors.is_empty() {
        return;
    }

    let mut after_cycle = 0;
    let mut last_errors = BTreeMap::new();

    loop {
        if let Ok(observation) = feed
            .wait_for_next_cycle(after_cycle, COLLECTOR_IDLE_WAIT)
            .await
        {
            after_cycle = observation.cycle();
        }

        collect_all(&feed, &mut collectors, &mut last_errors);
    }
}

fn collect_all(
    feed: &BlockFeed,
    collectors: &mut [BoxedBlockFeedCollector],
    last_errors: &mut BTreeMap<&'static str, String>,
) {
    for collector in collectors {
        let name = collector.name();

        match collector.collect(feed) {
            Ok(()) => {
                last_errors.remove(name);
            }
            Err(error) => {
                let error = error.to_string();

                if last_errors.get(name) != Some(&error) {
                    warn!(collector = name, %error, "block-feed collector failed");
                }

                last_errors.insert(name, error);
            }
        }
    }
}
