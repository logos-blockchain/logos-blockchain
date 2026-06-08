use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use lb_core::mantle::{TxHash, Utxo};
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
    common::wallet::{WalletBlockFeedTrackerError, WalletObservedBlock},
    cucumber::{
        TARGET,
        world::{
            SharedScannedTransactionHashes, SharedTrackedWallets, SharedWalletBlockFeedTracker,
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

pub struct CucumberWalletBlockFeed {
    provider: DynamicWalletBlockFeedSources,
    feed: BlockFeed,
    _runtime: ObservationRuntime<BlockFeedObserver>,
    _collectors: BlockFeedCollectorRuntime,
}

impl CucumberWalletBlockFeed {
    pub async fn start(
        wallets: SharedTrackedWallets,
        tracker: SharedWalletBlockFeedTracker,
        scanned_transaction_hashes: SharedScannedTransactionHashes,
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
            vec![Box::new(WalletBlockFeedStateCollector::new(
                wallets,
                tracker,
                scanned_transaction_hashes,
                genesis_utxos,
            ))],
        );

        Ok(Self {
            provider,
            feed,
            _runtime: runtime,
            _collectors: collectors,
        })
    }

    #[must_use]
    pub fn feed(&self) -> BlockFeed {
        self.feed.clone()
    }

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

struct WalletBlockFeedStateCollector {
    wallets: SharedTrackedWallets,
    tracker: SharedWalletBlockFeedTracker,
    scanned_transaction_hashes: SharedScannedTransactionHashes,
    genesis_utxos: Vec<Utxo>,
}

impl WalletBlockFeedStateCollector {
    const fn new(
        wallets: SharedTrackedWallets,
        tracker: SharedWalletBlockFeedTracker,
        scanned_transaction_hashes: SharedScannedTransactionHashes,
        genesis_utxos: Vec<Utxo>,
    ) -> Self {
        Self {
            wallets,
            tracker,
            scanned_transaction_hashes,
            genesis_utxos,
        }
    }

    fn collect_wallet_state(
        &self,
        feed: &BlockFeed,
    ) -> Result<(), WalletBlockFeedStateCollectorError> {
        let update_result = {
            let observed_blocks = {
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

            apply_observed_blocks_to_scanned_transaction_hashes(
                &self.scanned_transaction_hashes,
                &observed_blocks,
            );

            Ok::<_, WalletBlockFeedTrackerError>(())
        };

        match update_result {
            Ok(()) => Ok(()),
            Err(error) if error.requires_direct_backfill() => Ok(()),
            Err(error) => Err(error.into()),
        }
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

/// Applies the transaction hashes from the observed blocks to the shared set of
/// scanned transaction hashes, and logs the update.
pub fn apply_observed_blocks_to_scanned_transaction_hashes(
    scanned_transaction_hashes: &SharedScannedTransactionHashes,
    observed_blocks: &[WalletObservedBlock],
) {
    if !observed_blocks.is_empty() {
        let mut sink = scanned_transaction_hashes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = sink.len();
        for observed in observed_blocks {
            sink.extend(observed.transaction_hashes().iter().copied());
        }
        let total_transactions = sink.len();
        drop(sink);
        let new_transactions = total_transactions.saturating_sub(before);
        let new_blocks = observed_blocks
            .iter()
            .map(WalletObservedBlock::height)
            .collect::<Vec<_>>();
        info!(
            target: TARGET,
            "observed blocks={new_blocks:?}, new transactions={new_transactions} total recorded \
            transactions={total_transactions}",
        );
    }
}

/// Applies the transaction hashes from alist of provided transaction hashes to
/// the shared set of scanned transaction hashes, and logs the update.
pub fn apply_observed_hashes_to_scanned_transaction_hashes<S: ::std::hash::BuildHasher>(
    scanned_transaction_hashes: &SharedScannedTransactionHashes,
    transaction_hashes: &HashSet<TxHash, S>,
    num_of_blocks: Option<usize>,
) {
    let mut sink = scanned_transaction_hashes
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let before = sink.len();
    sink.extend(transaction_hashes);
    let after = sink.len();
    drop(sink);
    let new = after.saturating_sub(before);
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
