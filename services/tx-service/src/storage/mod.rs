use std::{collections::HashMap, hash::Hash, pin::Pin};

use async_trait::async_trait;
use futures::Stream;
use lb_cryptarchia_engine::Slot;
use lb_time::Era;

pub mod adapters;

#[async_trait]
pub trait MempoolStorageAdapter<RuntimeServiceId>: Send + Sync {
    type Item: Send;

    type Key: Send + Sync;

    type BlockId: Hash + Eq + Send + Sync;

    type Error: Send;

    fn new(
        current_era: Era,
        storage_relay: overwatch::services::relay::OutboundRelay<
            <lb_storage_service::StorageService<RuntimeServiceId> as overwatch::services::ServiceData>::Message,
        >,
    ) -> Self;

    async fn rotate_era(&mut self, era: Era);
    async fn complete_era_transition(&mut self, ended_era: Era);

    async fn store_item(&mut self, key: Self::Key, item: Self::Item) -> Result<(), Self::Error>;

    async fn get_items(
        &self,
        keys: &[Self::Key],
    ) -> Result<Pin<Box<dyn Stream<Item = Self::Item> + Send>>, Self::Error>;

    async fn remove_items(&mut self, keys: &[Self::Key]) -> Result<(), Self::Error>;

    /// Slots of the stored blocks among `block_ids`. Every block is looked
    /// up on its own, so one failed lookup does not lose the others.
    async fn get_block_slots(
        &self,
        block_ids: &[Self::BlockId],
    ) -> BlockSlotLookup<Self::BlockId, Self::Error>;
}

/// Outcome of a [`MempoolStorageAdapter::get_block_slots`] lookup. Blocks
/// that are simply not stored appear in neither set.
#[derive(Debug)]
pub struct BlockSlotLookup<BlockId, Error> {
    /// Slots of the blocks storage holds.
    pub slots: HashMap<BlockId, Slot>,
    /// Blocks whose lookup failed, with the failure.
    pub failed: Vec<(BlockId, Error)>,
}

impl<BlockId, Error> Default for BlockSlotLookup<BlockId, Error> {
    fn default() -> Self {
        Self {
            slots: HashMap::new(),
            failed: Vec::new(),
        }
    }
}
