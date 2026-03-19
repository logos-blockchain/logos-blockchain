use std::{
    collections::BTreeSet,
    fmt::Debug,
    hash::Hash,
    pin::Pin,
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};

use super::Status;
use crate::{
    backend::{MemPool, MempoolError, RecoverableMempool},
    metrics,
    storage::MempoolStorageAdapter,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PoolRecoveryState<Key>
where
    Key: Hash + Eq + Ord,
{
    pub pending_items: BTreeSet<Key>,
    pub last_item_timestamp: u64,
}

pub struct Mempool<BlockId, Item, Key, Storage, RuntimeServiceId> {
    pending_items: BTreeSet<Key>,
    last_item_timestamp: u64,
    storage_adapter: Storage,
    _phantom: std::marker::PhantomData<(BlockId, Item, RuntimeServiceId)>,
}

impl<BlockId, Item, Key, Storage, RuntimeServiceId> Debug
    for Mempool<BlockId, Item, Key, Storage, RuntimeServiceId>
where
    BlockId: Debug,
    Item: Debug,
    Key: Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mempool")
            .field("pending_items", &self.pending_items)
            .field("last_item_timestamp", &self.last_item_timestamp)
            .field("storage_adapter", &"<StorageAdapter>")
            .finish()
    }
}

#[async_trait]
impl<BlockId, Item, Key, Storage, RuntimeServiceId> MemPool
    for Mempool<BlockId, Item, Key, Storage, RuntimeServiceId>
where
    Key: Hash + Eq + Ord + Clone + Send + Sync + 'static,
    Item: Clone + Send + Sync + 'static + Serialize + for<'de> Deserialize<'de>,
    BlockId: Hash + Eq + Copy + Send + Sync + 'static + Serialize + for<'de> Deserialize<'de>,
    Storage:
        MempoolStorageAdapter<RuntimeServiceId, Key = Key, Item = Item> + Send + Sync + 'static,
    Storage::Error: Debug,
    RuntimeServiceId: Send + Sync,
{
    type Settings = ();
    type Item = Item;
    type Key = Key;
    type BlockId = BlockId;
    type Storage = Storage;

    fn new(_settings: Self::Settings, storage: Self::Storage) -> Self {
        Self {
            pending_items: BTreeSet::new(),
            last_item_timestamp: 0,
            storage_adapter: storage,
            _phantom: std::marker::PhantomData,
        }
    }

    async fn add_item<I: Into<Self::Item> + Send>(
        &mut self,
        key: Self::Key,
        item: I,
    ) -> Result<(), MempoolError> {
        if self.pending_items.contains(&key) {
            return Err(MempoolError::ExistingItem);
        }

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        if let Err(e) = self
            .storage_adapter
            .store_item(key.clone(), item.into())
            .await
        {
            tracing::warn!("Failed to store item in storage: {:?}", e);
        }

        self.pending_items.insert(key);
        self.last_item_timestamp = timestamp;

        metrics::mempool_transactions_added();
        metrics::mempool_transactions_pending(self.pending_items.len());

        Ok(())
    }

    async fn view(
        &self,
        _ancestor_hint: BlockId,
    ) -> Result<Pin<Box<dyn Stream<Item = Self::Item> + Send>>, MempoolError> {
        let keys: BTreeSet<Key> = self.pending_items.iter().cloned().collect();
        self.get_items_by_keys(keys.into_iter()).await
    }

    async fn get_items_by_keys<I>(
        &self,
        keys: I,
    ) -> Result<Pin<Box<dyn Stream<Item = Self::Item> + Send>>, MempoolError>
    where
        I: IntoIterator<Item = Self::Key> + Send,
    {
        let keys_set: BTreeSet<Self::Key> = keys.into_iter().collect();
        self.storage_adapter
            .get_items(&keys_set)
            .await
            .map_err(|e| MempoolError::StorageError(format!("{e:?}")))
    }

    async fn remove(&mut self, keys: &[Self::Key]) {
        let removed_count = keys.len();
        for key in keys {
            self.pending_items.remove(key);
        }

        metrics::mempool_transactions_removed(removed_count);
        metrics::mempool_transactions_pending(self.pending_items.len());

        if let Err(e) = self.storage_adapter.remove_items(keys).await {
            tracing::warn!("Failed to remove items from storage: {e:?}");
        }
    }

    fn pending_item_count(&self) -> usize {
        self.pending_items.len()
    }

    fn last_item_timestamp(&self) -> u64 {
        self.last_item_timestamp
    }

    fn status(&self, items: &[Self::Key]) -> Vec<Status> {
        items
            .iter()
            .map(|key| {
                if self.pending_items.contains(key) {
                    Status::Pending
                } else {
                    Status::Unknown
                }
            })
            .collect()
    }
}

impl<BlockId, Item, Key, Storage, RuntimeServiceId> RecoverableMempool
    for Mempool<BlockId, Item, Key, Storage, RuntimeServiceId>
where
    Key: Hash + Eq + Ord + Clone + Send + Sync + 'static + Serialize + for<'de> Deserialize<'de>,
    Item: Clone + Send + Sync + 'static + Serialize + for<'de> Deserialize<'de>,
    BlockId: Hash + Eq + Copy + Send + Sync + 'static + Serialize + for<'de> Deserialize<'de>,
    Storage:
        MempoolStorageAdapter<RuntimeServiceId, Key = Key, Item = Item> + Send + Sync + 'static,
    Storage::Error: Debug,
    RuntimeServiceId: Send + Sync,
{
    type RecoveryState = PoolRecoveryState<Key>;

    fn save(&self) -> Self::RecoveryState {
        PoolRecoveryState {
            pending_items: self.pending_items.clone(),
            last_item_timestamp: self.last_item_timestamp,
        }
    }

    fn recover(
        _settings: <Self as MemPool>::Settings,
        state: Self::RecoveryState,
        storage: <Self as MemPool>::Storage,
    ) -> Self {
        Self {
            pending_items: state.pending_items,
            last_item_timestamp: state.last_item_timestamp,
            storage_adapter: storage,
            _phantom: std::marker::PhantomData,
        }
    }
}
