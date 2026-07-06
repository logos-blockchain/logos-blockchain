use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    hash::BuildHasher,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use lb_core::mantle::{SignedMantleTx, TxHash, Utxo};
use lb_node::{HeaderId, Transaction as _};
use lb_testing_framework::{
    BlockFeed, BlockFeedCollector, BlockFeedCollectorRuntime, BlockFeedObserver, NodeHttpClient,
    named_block_feed_sources,
};
use testing_framework_core::{
    observation::{ObservationRuntime, ObservationRuntimeError, ObservedSource, SourceProvider},
    scenario::DynError,
};
use thiserror::Error;
use tracing::info;

use crate::{
    common::wallet::WalletBlockFeedTrackerError,
    cucumber::{
        TARGET,
        world::{
            SharedObservedTransactionHashes, SharedTrackedWallets, SharedWalletBlockFeedTracker,
        },
    },
};

#[derive(Debug, Error)]
pub enum CucumberWalletBlockFeedError {
    #[error("failed to start wallet block feed: {0}")]
    Start(#[from] ObservationRuntimeError),
    #[error("wallet block feed source lock is poisoned")]
    SourceLockPoisoned,
}

/// Runtime wallet feed used by Cucumber scenarios.
///
/// It observes blocks from dynamically registered node sources, updates tracked
/// wallet state, and records transaction hashes seen in chain blocks.
pub struct CucumberWalletBlockFeed {
    provider: DynamicWalletBlockFeedSources,
    feed: BlockFeed,
    _runtime: ObservationRuntime<BlockFeedObserver>,
    _collectors: BlockFeedCollectorRuntime,
}

impl CucumberWalletBlockFeed {
    /// Start the wallet block feed and its collectors.
    ///
    /// Sources are registered later as nodes start, so the feed can survive
    /// manual-cluster restarts and snapshot restore flows.
    pub async fn start(
        wallets: SharedTrackedWallets,
        tracker: SharedWalletBlockFeedTracker,
        observed_transaction_hashes: SharedObservedTransactionHashes,
        genesis_utxos: Vec<Utxo>,
    ) -> Result<Self, CucumberWalletBlockFeedError> {
        let provider = DynamicWalletBlockFeedSources::default();
        let runtime = ObservationRuntime::start(
            provider.clone(),
            BlockFeedObserver,
            BlockFeedObserver::config(),
        )
        .await?;
        let feed = BlockFeed::new(runtime.handle());
        let collectors = BlockFeedCollectorRuntime::start(
            feed.clone(),
            vec![
                Box::new(WalletBlockFeedStateCollector::new(
                    wallets,
                    tracker,
                    genesis_utxos,
                )),
                Box::new(TransactionHashCollector::new(observed_transaction_hashes)),
            ],
        );

        Ok(Self {
            provider,
            feed,
            _runtime: runtime,
            _collectors: collectors,
        })
    }

    #[must_use]
    /// Return a handle to the underlying block feed.
    pub fn feed(&self) -> BlockFeed {
        self.feed.clone()
    }

    /// Add or replace a node source observed by the wallet feed.
    pub fn register_source(
        &self,
        node_name: &str,
        client: NodeHttpClient,
    ) -> Result<(), CucumberWalletBlockFeedError> {
        self.provider.register_source(node_name, client)
    }
}

impl fmt::Debug for CucumberWalletBlockFeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CucumberWalletBlockFeed")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Error)]
enum WalletBlockFeedStateCollectorError {
    #[error("wallet feed tracker lock is poisoned")]
    TrackerLockPoisoned,
    #[error("wallet state lock is poisoned")]
    WalletLockPoisoned,
    #[error(transparent)]
    FeedState(#[from] WalletBlockFeedTrackerError),
}

impl WalletBlockFeedStateCollectorError {
    const fn requires_direct_backfill(&self) -> bool {
        matches!(self, Self::FeedState(error) if error.requires_direct_backfill())
    }
}

struct WalletBlockFeedStateCollector {
    wallets: SharedTrackedWallets,
    tracker: SharedWalletBlockFeedTracker,
    genesis_utxos: Vec<Utxo>,
}

impl WalletBlockFeedStateCollector {
    const fn new(
        wallets: SharedTrackedWallets,
        tracker: SharedWalletBlockFeedTracker,
        genesis_utxos: Vec<Utxo>,
    ) -> Self {
        Self {
            wallets,
            tracker,
            genesis_utxos,
        }
    }

    fn collect_wallet_state(
        &self,
        feed: &BlockFeed,
    ) -> Result<(), WalletBlockFeedStateCollectorError> {
        match self.apply_feed(feed) {
            Ok(()) => Ok(()),
            Err(error) if error.requires_direct_backfill() => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn apply_feed(&self, feed: &BlockFeed) -> Result<(), WalletBlockFeedStateCollectorError> {
        let _observed_blocks = {
            let mut tracker = self
                .tracker
                .lock()
                .map_err(|_| WalletBlockFeedStateCollectorError::TrackerLockPoisoned)?;
            let mut wallets = self
                .wallets
                .lock()
                .map_err(|_| WalletBlockFeedStateCollectorError::WalletLockPoisoned)?;

            tracker.apply_feed(&mut wallets, feed, &self.genesis_utxos)?
        };

        Ok(())
    }
}

impl BlockFeedCollector for WalletBlockFeedStateCollector {
    fn name(&self) -> &'static str {
        "wallet_state"
    }

    fn collect(&mut self, feed: &BlockFeed) -> Result<(), DynError> {
        self.collect_wallet_state(feed)?;
        Ok(())
    }
}

struct TransactionHashCollector {
    observed_transaction_hashes: SharedObservedTransactionHashes,
    observed_headers: HashSet<HeaderId>,
}

impl TransactionHashCollector {
    fn new(observed_transaction_hashes: SharedObservedTransactionHashes) -> Self {
        Self {
            observed_transaction_hashes,
            observed_headers: HashSet::new(),
        }
    }

    fn collect_transaction_hashes(&mut self, feed: &BlockFeed) {
        let mut hashes = HashSet::new();
        let mut new_blocks = 0usize;

        for record in feed.history() {
            for event in &record.events {
                if !self.observed_headers.insert(event.header) {
                    continue;
                }

                new_blocks += 1;
                hashes.extend(event.block.transactions.iter().map(SignedMantleTx::hash));
            }
        }

        if new_blocks == 0 {
            return;
        }

        record_observed_transaction_hashes(
            &self.observed_transaction_hashes,
            &hashes,
            Some(new_blocks),
        );
    }
}

impl BlockFeedCollector for TransactionHashCollector {
    fn name(&self) -> &'static str {
        "transaction_hashes"
    }

    fn collect(&mut self, feed: &BlockFeed) -> Result<(), DynError> {
        self.collect_transaction_hashes(feed);
        Ok(())
    }
}

#[derive(Clone, Default)]
struct DynamicWalletBlockFeedSources {
    sources: Arc<RwLock<BTreeMap<String, NodeHttpClient>>>,
}

impl DynamicWalletBlockFeedSources {
    fn register_source(
        &self,
        node_name: &str,
        client: NodeHttpClient,
    ) -> Result<(), CucumberWalletBlockFeedError> {
        self.sources
            .write()
            .map_err(|_| CucumberWalletBlockFeedError::SourceLockPoisoned)?
            .insert(node_name.to_owned(), client);

        Ok(())
    }
}

#[async_trait]
impl SourceProvider<NodeHttpClient> for DynamicWalletBlockFeedSources {
    async fn sources(&self) -> Result<Vec<ObservedSource<NodeHttpClient>>, DynError> {
        let sources = self
            .sources
            .read()
            .map_err(|_| CucumberWalletBlockFeedError::SourceLockPoisoned)?;

        Ok(named_block_feed_sources(sources.iter().map(
            |(node_name, client)| (node_name.clone(), client.clone()),
        )))
    }
}

/// Records transaction hashes observed in chain blocks and logs the update.
pub(crate) fn record_observed_transaction_hashes<S: BuildHasher>(
    observed_transaction_hashes: &SharedObservedTransactionHashes,
    transaction_hashes: &HashSet<TxHash, S>,
    num_of_blocks: Option<usize>,
) {
    let mut sink = observed_transaction_hashes
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let before = sink.len();
    sink.extend(transaction_hashes);
    let after = sink.len();
    drop(sink);
    let new = after.saturating_sub(before);
    if new > 0 {
        if let Some(num) = num_of_blocks
            && num > 0
        {
            info!(
                target: TARGET,
                "observed blocks={num}, new transactions={new} total recorded transactions={after}",
            );
        } else {
            info!(
                target: TARGET,
                "new transactions={new} total recorded transactions={after}",
            );
        }
    }
}
