use std::{cmp::Ordering, sync::Arc, time::SystemTime};

use lb_node::HeaderId;
use testing_framework_core::observation::{
    ObservationFailure, ObservationHandle, ObservationSnapshot,
};
use tokio::{
    sync::broadcast,
    time::{Duration, Instant, sleep},
};

use super::{BlockFeedObserver, BlockFeedSnapshot, BlockRecord, NodeHeadSnapshot};

/// Read-side handle for the Logos block observer.
#[derive(Clone)]
pub struct BlockFeed {
    handle: ObservationHandle<BlockFeedObserver>,
}

impl BlockFeed {
    /// Wraps one running block-feed observation handle.
    #[must_use]
    pub const fn new(handle: ObservationHandle<BlockFeedObserver>) -> Self {
        Self { handle }
    }

    /// Subscribes to future non-empty batches of newly observed blocks.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<BlockRecord>> {
        self.handle.subscribe()
    }

    /// Returns the latest materialized block-feed observation.
    #[must_use]
    pub fn latest_observation(&self) -> Option<BlockFeedObservation> {
        self.handle
            .latest_snapshot()
            .map(BlockFeedObservation::from_snapshot)
    }

    /// Returns the latest materialized block-feed snapshot.
    ///
    /// This is a cheap read over the last completed observation cycle; it does
    /// not wait for the observer to poll again.
    #[must_use]
    pub fn snapshot(&self) -> BlockFeedSnapshot {
        self.latest_observation()
            .map(|observation| observation.snapshot)
            .unwrap_or_default()
    }

    /// Returns retained non-empty event history.
    #[must_use]
    pub fn history(&self) -> Vec<Arc<BlockRecord>> {
        self.handle.history()
    }

    /// Returns the most recent observation error, if any.
    #[must_use]
    pub fn last_error(&self) -> Option<ObservationFailure> {
        self.handle.last_error()
    }

    /// Waits until a newer observation cycle is available.
    pub async fn wait_for_next_cycle(
        &self,
        after_cycle: u64,
        timeout: Duration,
    ) -> Result<BlockFeedObservation, BlockFeedWaitError> {
        let deadline = Instant::now() + timeout;

        loop {
            if let Some(observation) = self.latest_observation()
                && observation.cycle > after_cycle
            {
                return Ok(observation);
            }

            if Instant::now() >= deadline {
                let last_error = self.last_error().map_or_else(
                    || "no observation error recorded".to_owned(),
                    |error| error.message,
                );

                return Err(BlockFeedWaitError::Timeout {
                    after_cycle,
                    last_error,
                });
            }

            sleep(Duration::from_millis(100)).await;
        }
    }
}

/// One materialized block-feed observation with cycle and freshness metadata.
#[derive(Clone, Debug)]
pub struct BlockFeedObservation {
    cycle: u64,
    observed_at: SystemTime,
    snapshot: BlockFeedSnapshot,
}

impl BlockFeedObservation {
    fn from_snapshot(snapshot: ObservationSnapshot<BlockFeedSnapshot>) -> Self {
        Self {
            cycle: snapshot.cycle,
            observed_at: snapshot.observed_at,
            snapshot: snapshot.value,
        }
    }

    /// Returns the observation cycle.
    #[must_use]
    pub const fn cycle(&self) -> u64 {
        self.cycle
    }

    /// Returns the observation timestamp.
    #[must_use]
    pub const fn observed_at(&self) -> SystemTime {
        self.observed_at
    }

    /// Returns the underlying block-feed snapshot.
    #[must_use]
    pub const fn snapshot(&self) -> &BlockFeedSnapshot {
        &self.snapshot
    }

    /// Returns a concise summary for timeout and validation errors.
    #[must_use]
    pub fn summary(&self) -> String {
        self.snapshot.summary()
    }

    /// Returns the latest observed head for one node, if available.
    #[must_use]
    pub fn node_head(&self, node_name: &str) -> Option<&NodeHeadSnapshot> {
        self.snapshot.node_head(node_name)
    }

    /// Returns a chain view for one observed node, if available.
    #[must_use]
    pub fn node(&self, node_name: &str) -> Option<NodeChainView<'_>> {
        self.node_head(node_name).map(|node_head| NodeChainView {
            observation: self,
            node_head,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BlockFeedWaitError {
    #[error("timed out waiting for block-feed cycle after {after_cycle}; last_error={last_error}")]
    Timeout {
        after_cycle: u64,
        last_error: String,
    },
}

/// Query view over one node's canonical chain within one observation.
#[derive(Clone, Copy, Debug)]
pub struct NodeChainView<'a> {
    observation: &'a BlockFeedObservation,
    node_head: &'a NodeHeadSnapshot,
}

impl<'a> NodeChainView<'a> {
    /// Returns the full retained head view for this node.
    #[must_use]
    pub const fn head(&self) -> &'a NodeHeadSnapshot {
        self.node_head
    }

    /// Returns the node tip header id from this observation.
    #[must_use]
    pub const fn tip(&self) -> HeaderId {
        self.node_head.tip
    }

    /// Returns the node LIB header id from this observation.
    #[must_use]
    pub const fn lib(&self) -> HeaderId {
        self.node_head.lib
    }

    /// Returns the retained tip height, if the observer has seen it.
    #[must_use]
    pub const fn tip_height(&self) -> Option<u64> {
        self.node_head.tip_height
    }

    /// Returns the retained LIB height, if the observer has seen it.
    #[must_use]
    pub const fn lib_height(&self) -> Option<u64> {
        self.node_head.lib_height
    }

    /// Walks backward from the node tip until it reaches one target height.
    ///
    /// This only works within the observer's retained ancestry window.
    #[must_use]
    pub fn header_at_height(&self, height: u64) -> Option<HeaderId> {
        let mut cursor = self.tip();

        loop {
            let cursor_height = self.observation.snapshot.header_height(&cursor)?;
            match cursor_height.cmp(&height) {
                Ordering::Equal => return Some(cursor),
                Ordering::Less => return None,
                Ordering::Greater => {
                    let parent = self
                        .observation
                        .snapshot
                        .parent_edges
                        .get(&cursor)
                        .copied()?;
                    if parent == cursor {
                        return None;
                    }
                    cursor = parent;
                }
            }
        }
    }
}
