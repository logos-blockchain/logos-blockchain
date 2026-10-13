pub mod evictor;
pub mod policy;
pub mod pool;

use core::hash::Hash;
use std::pin::Pin;

use futures::Stream;
use lb_core::mantle::transactions::hash::PrefixedKey;
use lb_cryptarchia_engine::Slot;
use lb_time::Era;
pub use policy::{EvictableTx, EvictionPolicy, PowClaimEvictionSettings};
pub use pool::{DEFAULT_TX_TTL, Mempool, MempoolSettings, PoolRecoveryState};
use serde::{Deserialize, Serialize};

#[derive(thiserror::Error, Debug)]
pub enum MempoolError {
    #[error("Item already in mempool")]
    ExistingItem,
    #[error("Item is too large: {size} bytes exceeds maximum {max} bytes")]
    ItemTooLarge { size: usize, max: usize },
    #[error("Item can no longer be included in a block")]
    Expired,
    #[error("Storage operation failed: {0}")]
    StorageError(String),
    #[error(transparent)]
    DynamicPoolError(#[from] overwatch::DynError),
}

#[async_trait::async_trait]
pub trait MemPool {
    type Settings: Send;
    type Item: Send;
    type Key: Send + Sync + Clone + Ord + PrefixedKey;
    type BlockId: Send;
    type Storage: Send;

    /// Construct a new empty pool with storage
    fn new(settings: Self::Settings, current_era: Era, storage: Self::Storage) -> Self;

    async fn enter_era(&mut self, era: Era);

    /// Add a new item to the mempool, for example because we received it from
    /// the network. The item is stored in external storage.
    async fn add_item<I: Into<Self::Item> + Send>(
        &mut self,
        key: Self::Key,
        item: I,
    ) -> Result<(), MempoolError>;

    /// Return a view over items contained in the mempool.
    /// Implementations should provide *at least* all the items which have not
    /// been marked as in a block.
    /// The hint on the ancestor *can* be used by the implementation to display
    /// additional items that were not included up to that point if
    /// available.
    async fn view(
        &self,
        ancestor_hint: Self::BlockId,
    ) -> Result<Pin<Box<dyn Stream<Item = Self::Item> + Send>>, MempoolError>;

    /// Keys currently in the mempool sharing `prefix`.
    fn keys_by_prefix(
        &self,
        prefix: &<Self::Key as PrefixedKey>::Prefix,
    ) -> impl Iterator<Item = &Self::Key> + '_;

    /// Get multiple items by their keys from the mempool via storage lookup
    async fn get_items_by_keys<I>(
        &self,
        keys: I,
    ) -> Result<Pin<Box<dyn Stream<Item = Self::Item> + Send>>, MempoolError>
    where
        I: IntoIterator<Item = Self::Key> + Send;

    /// Remove items from the mempool..
    async fn remove(&mut self, items: &[Self::Key]);

    /// The chain processed block `block_id` at `slot`. This is the pool's
    /// only notion of chain time, so it also triggers eviction.
    async fn on_processed_block(&mut self, block_id: Self::BlockId, slot: Slot);

    fn pending_item_count(&self) -> usize;
    fn last_item_timestamp(&self) -> u64;

    // Return the status of a set of items.
    // This is a best effort attempt, and implementations are free to return
    // `Unknown` for all of them.
    fn status(&self, items: &[Self::Key]) -> Vec<Status>;
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum Status {
    /// Unknown status
    Unknown,
    /// Pending status
    Pending,
}

/// Trait for mempools that can be recovered from saved state
pub trait RecoverableMempool: MemPool {
    type RecoveryState: Send + Sync + Serialize + for<'de> Deserialize<'de>;

    /// Save current state for recovery
    fn save(&self) -> Self::RecoveryState;

    /// Recover from saved state with storage
    fn recover(
        settings: <Self as MemPool>::Settings,
        current_era: Era,
        state: Self::RecoveryState,
        storage: <Self as MemPool>::Storage,
    ) -> Self;
}
