use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use lb_core::mantle::Utxo;
use lb_testing_framework::{
    BlockFeed, BlockFeedCollector, BlockFeedCollectorRuntime, BlockFeedObserver, NodeHttpClient,
    named_block_feed_sources,
};
use testing_framework_core::{
    observation::{ObservationRuntime, ObservationRuntimeError, ObservedSource, SourceProvider},
    scenario::DynError,
};
use thiserror::Error;

use crate::{
    common::wallet::WalletBlockFeedTrackerError,
    cucumber::world::{SharedTrackedWallets, SharedWalletBlockFeedTracker},
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
        let update_result = {
            let mut tracker = self
                .tracker
                .lock()
                .map_err(|_| WalletBlockFeedStateCollectorError::TrackerLockPoisoned)?;
            let mut wallets = self
                .wallets
                .lock()
                .map_err(|_| WalletBlockFeedStateCollectorError::WalletLockPoisoned)?;

            tracker.apply_feed(&mut wallets, feed, &self.genesis_utxos)
        };

        match update_result {
            Ok(_) => Ok(()),
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
