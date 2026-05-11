use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Debug,
    hash::Hash,
    pin::Pin,
    time::{Duration, SystemTime, UNIX_EPOCH},
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

const REMOVED_ITEM_GRACE_PERIOD: Duration = Duration::from_mins(10);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PoolRecoveryState<Key>
where
    Key: Hash + Eq + Ord,
{
    pub pending_items: BTreeSet<Key>,
    pub removed_items: BTreeMap<Key, u64>,
    pub last_item_timestamp: u64,
}

pub struct Mempool<BlockId, Item, Key, Storage, RuntimeServiceId> {
    pending_items: BTreeSet<Key>,
    removed_items: BTreeMap<Key, u64>,
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
            .field("removed_items", &self.removed_items)
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
            removed_items: BTreeMap::new(),
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
        self.prune_removed_items().await;

        if self.pending_items.contains(&key) {
            return Err(MempoolError::ExistingItem);
        }

        let timestamp = current_timestamp_millis();

        if let Err(e) = self
            .storage_adapter
            .store_item(key.clone(), item.into())
            .await
        {
            tracing::warn!("Failed to store item in storage: {:?}", e);
        }

        self.removed_items.remove(&key);
        self.pending_items.insert(key);
        self.last_item_timestamp = timestamp;
        tracing::debug!(
            "Added item to mempool; pending_items={}, last_item_timestamp={}",
            self.pending_items.len(),
            self.last_item_timestamp
        );

        metrics::mempool_transactions_added();
        metrics::mempool_transactions_pending(self.pending_items.len());

        Ok(())
    }

    async fn view(
        &self,
        _ancestor_hint: BlockId,
    ) -> Result<Pin<Box<dyn Stream<Item = Self::Item> + Send>>, MempoolError> {
        let keys: BTreeSet<Key> = self.pending_items.iter().cloned().collect();
        self.get_items_by_keys(keys).await
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
        self.prune_removed_items().await;

        let removed_count = keys.len();
        let removed_at = current_timestamp_millis();

        for key in keys {
            self.pending_items.remove(key);
            self.removed_items.insert(key.clone(), removed_at);
        }
        log_removed_items(removed_count, self.pending_items.len());

        metrics::mempool_transactions_removed(removed_count);
        metrics::mempool_transactions_pending(self.pending_items.len());
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
            removed_items: self.removed_items.clone(),
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
            removed_items: state.removed_items,
            last_item_timestamp: state.last_item_timestamp,
            storage_adapter: storage,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<BlockId, Item, Key, Storage, RuntimeServiceId>
    Mempool<BlockId, Item, Key, Storage, RuntimeServiceId>
where
    Key: Hash + Eq + Ord + Clone + Send + Sync + 'static,
    Item: Clone + Send + Sync + 'static + Serialize + for<'de> Deserialize<'de>,
    BlockId: Hash + Eq + Copy + Send + Sync + 'static + Serialize + for<'de> Deserialize<'de>,
    Storage:
        MempoolStorageAdapter<RuntimeServiceId, Key = Key, Item = Item> + Send + Sync + 'static,
    Storage::Error: Debug,
    RuntimeServiceId: Send + Sync,
{
    async fn prune_removed_items(&mut self) {
        let now = current_timestamp_millis();
        let grace_period_millis = REMOVED_ITEM_GRACE_PERIOD.as_millis() as u64;

        let expired_keys = self
            .removed_items
            .iter()
            .filter(|(_, removed_at)| now.saturating_sub(**removed_at) >= grace_period_millis)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();

        if expired_keys.is_empty() {
            return;
        }

        if let Err(e) = self.storage_adapter.remove_items(&expired_keys).await {
            tracing::warn!("Failed to prune removed items from storage: {e:?}");
            return;
        }

        for key in expired_keys {
            self.removed_items.remove(&key);
        }
    }
}

fn current_timestamp_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn log_removed_items(removed_count: usize, pending_items: usize) {
    if removed_count == 0 {
        tracing::trace!(
            "Removed {removed_count} items from mempool; pending_items={pending_items}"
        );
    } else {
        tracing::debug!(
            "Removed {removed_count} items from mempool; pending_items={pending_items}"
        );
    }
}
