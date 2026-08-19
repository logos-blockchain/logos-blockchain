use std::{
    collections::{BTreeMap, HashMap, hash_map::Entry},
    fmt::Debug,
    hash::Hash,
    pin::Pin,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use futures::Stream;
use indexmap::{IndexMap, IndexSet};
use lb_core::mantle::transactions::hash::PrefixedKey;
use lb_log_targets::mempool;
use serde::{Deserialize, Serialize};

use super::Status;
use crate::{
    backend::{MemPool, MempoolError, RecoverableMempool, evictor::Evictor},
    metrics,
    storage::MempoolStorageAdapter,
};

const REMOVED_ITEM_GRACE_PERIOD: Duration = Duration::from_mins(10);
const LOG_TARGET: &str = mempool::POOL;

/// Default time a pending transaction may stay in the pool before eviction.
pub const DEFAULT_TX_TTL: Duration = Duration::from_hours(24);

/// Settings for the [`Mempool`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MempoolSettings {
    /// How long a pending transaction is allowed to stay in the pool before
    /// it is evicted. `None` disables expiry-based eviction.
    #[serde(default = "default_tx_ttl")]
    pub tx_ttl: Option<Duration>,
}

impl Default for MempoolSettings {
    fn default() -> Self {
        Self {
            tx_ttl: default_tx_ttl(),
        }
    }
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "Serde default for an `Option` field."
)]
const fn default_tx_ttl() -> Option<Duration> {
    Some(DEFAULT_TX_TTL)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PoolRecoveryState<Key>
where
    Key: Hash + Eq + Ord,
{
    /// Pending item keys mapped to their insertion timestamp in milliseconds.
    pub pending_items: IndexMap<Key, u64>,
    pub removed_items: BTreeMap<Key, u64>,
    pub last_item_timestamp: u64,
}

pub struct Mempool<BlockId, Item, Key, Storage, RuntimeServiceId>
where
    Key: PrefixedKey,
{
    pending_items: IndexSet<Key>,
    /// Secondary index over [`Self::pending_items`], keyed by the hash prefix a
    /// block proposal would use to refer to a transaction.
    ///
    /// Buckets are plain vectors: a 128-bit prefix cannot be made to collide at
    /// any feasible cost, so every one holds a single key in practice. A set
    /// would cost a hash table per bucket to deduplicate keys that are already
    /// unique.
    by_prefix: HashMap<Key::Prefix, Vec<Key>>,
    removed_items: BTreeMap<Key, u64>,
    last_item_timestamp: u64,
    evictor: Evictor<Key>,
    storage_adapter: Storage,
    _phantom: std::marker::PhantomData<(BlockId, Item, RuntimeServiceId)>,
}

impl<BlockId, Item, Key, Storage, RuntimeServiceId> Debug
    for Mempool<BlockId, Item, Key, Storage, RuntimeServiceId>
where
    BlockId: Debug,
    Item: Debug,
    Key: Debug + PrefixedKey<Prefix: Debug>,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mempool")
            .field("pending_items", &self.pending_items)
            .field("by_prefix", &self.by_prefix)
            .field("removed_items", &self.removed_items)
            .field("last_item_timestamp", &self.last_item_timestamp)
            .field("storage_adapter", &"<StorageAdapter>")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl<BlockId, Item, Key, Storage, RuntimeServiceId> MemPool
    for Mempool<BlockId, Item, Key, Storage, RuntimeServiceId>
where
    Key: Hash
        + Eq
        + Ord
        + Clone
        + Send
        + Sync
        + PrefixedKey<Prefix: Eq + Hash + Send + Sync>
        + 'static,
    Item: Clone + Send + Sync + 'static + Serialize + for<'de> Deserialize<'de>,
    BlockId: Hash + Eq + Copy + Send + Sync + 'static + Serialize + for<'de> Deserialize<'de>,
    Storage:
        MempoolStorageAdapter<RuntimeServiceId, Key = Key, Item = Item> + Send + Sync + 'static,
    Storage::Error: Debug,
    RuntimeServiceId: Send + Sync,
{
    type Settings = MempoolSettings;
    type Item = Item;
    type Key = Key;
    type BlockId = BlockId;
    type Storage = Storage;

    fn new(settings: Self::Settings, storage: Self::Storage) -> Self {
        Self {
            pending_items: IndexSet::new(),
            by_prefix: HashMap::new(),
            removed_items: BTreeMap::new(),
            last_item_timestamp: 0,
            evictor: Evictor::new(&settings),
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

        self.storage_adapter
            .store_item(key.clone(), item.into())
            .await
            .map_err(|e| MempoolError::StorageError(format!("{e:?}")))?;

        self.removed_items.remove(&key);
        self.index_by_prefix(&key);
        self.evictor.on_add(key.clone(), timestamp);
        self.pending_items.insert(key);
        self.last_item_timestamp = timestamp;
        tracing::debug!(
            target: LOG_TARGET,
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
        self.get_items_by_keys(self.pending_items.iter().cloned())
            .await
    }

    fn keys_by_prefix(
        &self,
        prefix: &<Self::Key as PrefixedKey>::Prefix,
    ) -> impl Iterator<Item = &Self::Key> + '_ {
        // Returns empty iterator if prefix is not found.
        self.by_prefix.get(prefix).into_iter().flatten()
    }

    async fn get_items_by_keys<I>(
        &self,
        keys: I,
    ) -> Result<Pin<Box<dyn Stream<Item = Self::Item> + Send>>, MempoolError>
    where
        I: IntoIterator<Item = Self::Key> + Send,
    {
        let ordered_keys = keys.into_iter().collect::<Vec<_>>();

        self.storage_adapter
            .get_items(&ordered_keys)
            .await
            .map_err(|e| MempoolError::StorageError(format!("{e:?}")))
    }

    async fn remove(&mut self, keys: &[Self::Key]) {
        self.prune_removed_items().await;

        let removed_at = current_timestamp_millis();

        self.retire(keys.iter().cloned(), removed_at, "removed");

        // `remove` is called once per applied canonical block, so it doubles
        // as the periodic trigger for eviction — without it the pool has no
        // clock.
        self.retire(
            self.evictor.select_evictions(removed_at),
            removed_at,
            "evicted",
        );

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
    Key: Hash
        + Eq
        + Ord
        + Clone
        + Send
        + Sync
        + PrefixedKey<Prefix: Eq + Hash + Send + Sync>
        + 'static
        + Serialize
        + for<'de> Deserialize<'de>,
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
            pending_items: self.evictor.save(),
            removed_items: self.removed_items.clone(),
            last_item_timestamp: self.last_item_timestamp,
        }
    }

    fn recover(
        settings: <Self as MemPool>::Settings,
        state: Self::RecoveryState,
        storage: <Self as MemPool>::Storage,
    ) -> Self {
        // `by_prefix` is derived, so it is rebuilt rather than restored.
        let mut by_prefix: HashMap<Key::Prefix, Vec<Key>> =
            HashMap::with_capacity(state.pending_items.len());
        for key in state.pending_items.keys() {
            by_prefix
                .entry(key.key_prefix())
                .or_default()
                .push(key.clone());
        }

        Self {
            pending_items: state.pending_items.keys().cloned().collect(),
            by_prefix,
            removed_items: state.removed_items,
            last_item_timestamp: state.last_item_timestamp,
            evictor: Evictor::recover(&settings, state.pending_items),
            storage_adapter: storage,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<BlockId, Item, Key, Storage, RuntimeServiceId>
    Mempool<BlockId, Item, Key, Storage, RuntimeServiceId>
where
    Key: Hash + Eq + Ord + Clone + Send + Sync + PrefixedKey<Prefix: Eq + Hash> + 'static,
    Item: Clone + Send + Sync + 'static + Serialize + for<'de> Deserialize<'de>,
    BlockId: Hash + Eq + Copy + Send + Sync + 'static + Serialize + for<'de> Deserialize<'de>,
    Storage:
        MempoolStorageAdapter<RuntimeServiceId, Key = Key, Item = Item> + Send + Sync + 'static,
    Storage::Error: Debug,
    RuntimeServiceId: Send + Sync,
{
    /// Record `key` in the prefix index, under the prefix a block proposal
    /// would use to refer to it.
    fn index_by_prefix(&mut self, key: &Key) {
        self.by_prefix
            .entry(key.key_prefix())
            .or_default()
            .push(key.clone());
    }

    /// Drop `key` from the prefix index, dropping the bucket with it once it
    /// holds nothing.
    fn unindex_by_prefix(&mut self, key: &Key) {
        let Entry::Occupied(mut bucket) = self.by_prefix.entry(key.key_prefix()) else {
            return;
        };

        // Bucket order is irrelevant — only reference order matters, and that
        // comes from a block proposal — so a swap-remove is fine to avoid
        // shifting the remaining elements around.
        if let Some(position) = bucket.get().iter().position(|held| held == key) {
            bucket.get_mut().swap_remove(position);
        }

        if bucket.get().is_empty() {
            bucket.remove();
        }
    }

    /// The single exit for pending items: every removal — included in a
    /// block or evicted — goes through here so the evictor stays in sync.
    fn retire(&mut self, keys: impl IntoIterator<Item = Key>, at: u64, cause: &str) {
        let mut count = 0usize;
        for key in keys {
            self.pending_items.shift_remove(&key);
            self.unindex_by_prefix(&key);
            self.evictor.on_remove(&key);
            self.removed_items.insert(key, at);
            count += 1;
        }

        log_retired_items(count, cause, self.pending_items.len());
        metrics::mempool_transactions_removed(count);
    }

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
            tracing::warn!(target: LOG_TARGET, "Failed to prune removed items from storage: {e:?}");
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

fn log_retired_items(count: usize, cause: &str, pending_items: usize) {
    if count == 0 {
        tracing::trace!(
            target: LOG_TARGET,
            "{cause}: 0 items from mempool; pending_items={pending_items}"
        );
    } else {
        tracing::debug!(
            target: LOG_TARGET,
            "{cause}: {count} items from mempool; pending_items={pending_items}"
        );
    }
}
