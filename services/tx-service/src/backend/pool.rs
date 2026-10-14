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
use lb_core::mantle::{
    traits::{Hashable, MantleTx},
    transactions::hash::PrefixedKey,
};
use lb_cryptarchia_engine::Slot;
use lb_log_targets::mempool;
use serde::{Deserialize, Serialize};

use super::Status;
use crate::{
    backend::{
        MemPool, MempoolError, RecoverableMempool,
        evictor::Evictor,
        policy::{BlockSlots, EvictableTx as _, PowClaimEvictionSettings},
    },
    metrics,
    storage::{BlockSlotLookup, MempoolStorageAdapter},
};

const REMOVED_ITEM_GRACE_PERIOD: Duration = Duration::from_mins(10);
const LOG_TARGET: &str = mempool::POOL;

/// Default time a pending transaction may stay in the pool before eviction.
pub const DEFAULT_TX_TTL: Duration = Duration::from_hours(9);

/// Settings for the [`Mempool`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MempoolSettings {
    /// How long a pending transaction is allowed to stay in the pool before
    /// it is evicted. `None` disables expiry-based eviction.
    #[serde(default = "default_tx_ttl")]
    pub tx_ttl: Option<Duration>,
    /// Eviction of transactions carrying an expired `PoW` reward claim.
    /// `None` disables it.
    #[serde(default)]
    pub pow_claim: Option<PowClaimEvictionSettings>,
}

impl Default for MempoolSettings {
    fn default() -> Self {
        Self {
            tx_ttl: default_tx_ttl(),
            pow_claim: None,
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

/// Transactions are keyed by their hash.
pub type ItemKey<Item> = <Item as Hashable>::Hash;

pub struct Mempool<BlockId, Item, Storage, RuntimeServiceId>
where
    Item: Hashable<Hash: PrefixedKey>,
{
    pending_items: IndexSet<ItemKey<Item>>,
    /// Secondary index over [`Self::pending_items`], keyed by the hash prefix a
    /// block proposal would use to refer to a transaction.
    ///
    /// Buckets are plain vectors: a 128-bit prefix cannot be made to collide at
    /// any feasible cost, so every one holds a single key in practice. A set
    /// would cost a hash table per bucket to deduplicate keys that are already
    /// unique.
    by_prefix: HashMap<<ItemKey<Item> as PrefixedKey>::Prefix, Vec<ItemKey<Item>>>,
    removed_items: BTreeMap<ItemKey<Item>, u64>,
    last_item_timestamp: u64,
    evictor: Evictor<Item, BlockId>,
    storage_adapter: Storage,
    _phantom: std::marker::PhantomData<(BlockId, Item, RuntimeServiceId)>,
}

impl<BlockId, Item, Storage, RuntimeServiceId> Debug
    for Mempool<BlockId, Item, Storage, RuntimeServiceId>
where
    BlockId: Debug,
    Item: Debug + Hashable<Hash: Debug + PrefixedKey<Prefix: Debug>>,
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
impl<BlockId, Item, Storage, RuntimeServiceId> MemPool
    for Mempool<BlockId, Item, Storage, RuntimeServiceId>
where
    Item: Clone + Send + Sync + 'static + Serialize + for<'de> Deserialize<'de> + MantleTx,
    ItemKey<Item>: Ord + PrefixedKey<Prefix: Eq + Hash + Send + Sync>,
    BlockId: Hash
        + Eq
        + Copy
        + Debug
        + Send
        + Sync
        + 'static
        + Serialize
        + for<'de> Deserialize<'de>
        + From<[u8; 32]>,
    Storage: MempoolStorageAdapter<RuntimeServiceId, Key = ItemKey<Item>, Item = Item, BlockId = BlockId>
        + Send
        + Sync
        + 'static,
    Storage::Error: Debug,
    RuntimeServiceId: Send + Sync,
{
    type Settings = MempoolSettings;
    type Item = Item;
    type Key = ItemKey<Item>;
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
        let item = item.into();

        // By the time a transaction arrives the chain may already be past the
        // point where a block could include it; such a transaction is refused
        // rather than admitted and evicted on the next sweep.
        let block_slots = self.resolve_claim_anchors(&item).await;
        if self.evictor.is_expired(&item, timestamp, &block_slots) {
            return Err(MempoolError::Expired);
        }

        self.storage_adapter
            .store_item(key, item.clone())
            .await
            .map_err(|e| MempoolError::StorageError(format!("{e:?}")))?;

        self.removed_items.remove(&key);
        self.index_by_prefix(&key);
        self.evictor.on_add(&key, &item, timestamp, &block_slots);
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
        self.get_items_by_keys(self.pending_items.iter().copied())
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

        self.retire(keys.iter().copied(), removed_at, "removed");

        // Removals come from applied canonical blocks and from leaders
        // dropping unselectable transactions, so they double as a trigger for
        // eviction alongside processed-block notifications.
        self.retire(
            self.evictor.select_evictions(removed_at),
            removed_at,
            "evicted",
        );

        metrics::mempool_transactions_pending(self.pending_items.len());
    }

    async fn on_processed_block(&mut self, block_id: BlockId, slot: Slot) {
        self.prune_removed_items().await;

        self.evictor.on_processed_block(&block_id, slot);

        let now = current_timestamp_millis();
        self.retire(self.evictor.select_evictions(now), now, "evicted");

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

impl<BlockId, Item, Storage, RuntimeServiceId> RecoverableMempool
    for Mempool<BlockId, Item, Storage, RuntimeServiceId>
where
    Item: Clone + Send + Sync + 'static + Serialize + for<'de> Deserialize<'de> + MantleTx,
    ItemKey<Item>:
        Ord + PrefixedKey<Prefix: Eq + Hash + Send + Sync> + Serialize + for<'de> Deserialize<'de>,
    BlockId: Hash
        + Eq
        + Copy
        + Debug
        + Send
        + Sync
        + 'static
        + Serialize
        + for<'de> Deserialize<'de>
        + From<[u8; 32]>,
    Storage: MempoolStorageAdapter<RuntimeServiceId, Key = ItemKey<Item>, Item = Item, BlockId = BlockId>
        + Send
        + Sync
        + 'static,
    Storage::Error: Debug,
    RuntimeServiceId: Send + Sync,
{
    type RecoveryState = PoolRecoveryState<ItemKey<Item>>;

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
        let mut by_prefix: HashMap<<ItemKey<Item> as PrefixedKey>::Prefix, Vec<ItemKey<Item>>> =
            HashMap::with_capacity(state.pending_items.len());
        for key in state.pending_items.keys() {
            by_prefix.entry(key.key_prefix()).or_default().push(*key);
        }

        Self {
            pending_items: state.pending_items.keys().copied().collect(),
            by_prefix,
            removed_items: state.removed_items,
            last_item_timestamp: state.last_item_timestamp,
            evictor: Evictor::recover(&settings, state.pending_items),
            storage_adapter: storage,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<BlockId, Item, Storage, RuntimeServiceId> Mempool<BlockId, Item, Storage, RuntimeServiceId>
where
    Item: Clone + Send + Sync + 'static + Serialize + for<'de> Deserialize<'de> + MantleTx,
    ItemKey<Item>: Ord + PrefixedKey<Prefix: Eq + Hash + Send + Sync>,
    BlockId: Hash
        + Eq
        + Copy
        + Debug
        + Send
        + Sync
        + 'static
        + Serialize
        + for<'de> Deserialize<'de>
        + From<[u8; 32]>,
    Storage: MempoolStorageAdapter<RuntimeServiceId, Key = ItemKey<Item>, Item = Item, BlockId = BlockId>
        + Send
        + Sync
        + 'static,
    Storage::Error: Debug,
    RuntimeServiceId: Send + Sync,
{
    /// Record `key` in the prefix index, under the prefix a block proposal
    /// would use to refer to it.
    fn index_by_prefix(&mut self, key: &ItemKey<Item>) {
        self.by_prefix
            .entry(key.key_prefix())
            .or_default()
            .push(*key);
    }

    /// Drop `key` from the prefix index, dropping the bucket with it once it
    /// holds nothing.
    fn unindex_by_prefix(&mut self, key: &ItemKey<Item>) {
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
    fn retire(&mut self, keys: impl IntoIterator<Item = ItemKey<Item>>, at: u64, cause: &str) {
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

    /// Slots of the blocks the `PoW` reward claims in `item` are anchored
    /// to, for those storage already holds. The rest resolve once the chain
    /// reports the block processed; that includes anchors whose lookup
    /// failed, which are logged and left to that notification or the TTL.
    async fn resolve_claim_anchors(&self, item: &Item) -> BlockSlots<BlockId> {
        // Collected up front: the op iterator borrows `item` and is not `Send`.
        let anchors = item
            .pow_reward_claims()
            .map(|claim| BlockId::from(claim.block_hash))
            .collect::<IndexSet<_>>();
        if anchors.is_empty() {
            return BlockSlots::new();
        }
        let anchors = anchors.into_iter().collect::<Vec<_>>();
        let BlockSlotLookup { slots, failed } =
            self.storage_adapter.get_block_slots(&anchors).await;
        for (anchor, error) in failed {
            tracing::warn!(
                target: LOG_TARGET,
                "Failed to look up the anchor block of a PoW reward claim; \
                 the claim waits for the block to be processed: \
                 block_id={anchor:?}, error={error:?}"
            );
        }
        slots
    }

    async fn prune_removed_items(&mut self) {
        let now = current_timestamp_millis();
        let grace_period_millis = REMOVED_ITEM_GRACE_PERIOD.as_millis() as u64;

        let expired_keys = self
            .removed_items
            .iter()
            .filter(|(_, removed_at)| now.saturating_sub(**removed_at) >= grace_period_millis)
            .map(|(key, _)| *key)
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
