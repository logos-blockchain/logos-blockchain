pub mod evictor;
pub mod policy;
pub mod pool;

use core::hash::Hash;
use std::{collections::HashMap, pin::Pin};

use futures::Stream;
use lb_core::mantle::transactions::hash::PrefixedKey;
use lb_cryptarchia_engine::Slot;
use lb_time::Era;
pub use policy::{EvictableTx, EvictionPolicy, PowClaimEvictionSettings};
pub use pool::{
    DEFAULT_TX_TTL, IncludedItem, Mempool, MempoolSettings, PoolRecoveryState, RemovedItem,
};
use serde::{Deserialize, Serialize};

/// Admission time of a transaction, as a Unix timestamp in milliseconds.
pub type AdmissionTime = u64;

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

    async fn rotate_era(&mut self, era: Era);
    async fn complete_era_transition(&mut self, ended_era: Era);

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

    /// The canonical chain was updated, either by growing with a new block or
    /// by a reorg.
    ///
    /// - `entered`: items carried by the blocks that entered the canonical
    ///   chain, each mapped to the slot of the block carrying it. They are
    ///   retired, and remain available for reference resolution until that
    ///   block becomes immutable.
    /// - `reorged`: items carried by the blocks that left the canonical chain
    ///   and not by the blocks now in it. They are no longer retained as
    ///   included, and are admitted again. Empty when the chain only grows.
    ///
    /// Returns the reorged items admitted again, for the caller to broadcast.
    async fn on_canonical_chain_updated(
        &mut self,
        entered: HashMap<Self::Key, Slot>,
        reorged: Vec<Self::Item>,
    ) -> Vec<Self::Item>;

    /// The chain processed block `block_id` at `slot`, and its latest
    /// immutable block is at `lib_slot`. This is the pool's only notion of
    /// chain time, so it also triggers eviction, and the release of the items
    /// whose block became immutable.
    async fn on_processed_block(&mut self, block_id: Self::BlockId, slot: Slot, lib_slot: Slot);

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
