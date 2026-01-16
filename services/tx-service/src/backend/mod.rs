pub mod pool;

use std::pin::Pin;

use futures::Stream;
pub use pool::{Mempool, PoolRecoveryState};
use serde::{Deserialize, Serialize};

#[derive(thiserror::Error, Debug)]
pub enum MempoolError {
    #[error("Item already in mempool")]
    ExistingItem,
    #[error("Storage operation failed: {0}")]
    StorageError(String),
    #[error(transparent)]
    DynamicPoolError(#[from] overwatch::DynError),
}

#[async_trait::async_trait]
pub trait MemPool {
    type Settings: Send;
    type Item: Send;
    type Key: Send + Sync + Clone + Ord;
    type BlockId: Send;
    type Storage: Send;

    /// Construct a new empty pool with storage
    fn new(settings: Self::Settings, storage: Self::Storage) -> Self;

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

    /// Get multiple items by their keys from the mempool via storage lookup
    async fn get_items_by_keys<I>(
        &self,
        keys: I,
    ) -> Result<Pin<Box<dyn Stream<Item = Self::Item> + Send>>, MempoolError>
    where
        I: IntoIterator<Item = Self::Key> + Send;

    /// Record that a set of items were included in a block
    fn mark_in_block(&mut self, items: &[Self::Key], block: Self::BlockId);

    /// Signal that a set of transactions can't be possibly requested anymore
    /// and can be discarded.
    async fn prune(&mut self, items: &[Self::Key]);

    fn pending_item_count(&self) -> usize;
    fn last_item_timestamp(&self) -> u64;

    // Return the status of a set of items.
    // This is a best effort attempt, and implementations are free to return
    // `Unknown` for all of them.
    fn status(&self, items: &[Self::Key]) -> Vec<Status<Self::BlockId>>;
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum Status<BlockId> {
    /// Unknown status
    Unknown,
    /// Pending status
    Pending,
    /// Rejected status
    Rejected,
    /// Accepted status
    ///
    /// The block id of the block that contains the item
    #[cfg_attr(
        feature = "openapi",
        schema(
            example = "e.g. 0x000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
        )
    )]
    InBlock { block: BlockId },
}

/// Trait for mempools that can be recovered from saved state
pub trait RecoverableMempool: MemPool {
    type RecoveryState: Send + Sync + Serialize + for<'de> Deserialize<'de>;

    /// Save current state for recovery
    fn save(&self) -> Self::RecoveryState;

    /// Recover from saved state with storage
    fn recover(
        settings: <Self as MemPool>::Settings,
        state: Self::RecoveryState,
        storage: <Self as MemPool>::Storage,
    ) -> Self;
}
