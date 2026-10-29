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
use lb_time::{Era, era::EraSchedule};
use serde::{Deserialize, Serialize};

use super::Status;
use crate::{
    backend::{
        AdmissionTime, MemPool, MempoolError, RecoverableMempool,
        evictor::Evictor,
        policy::{BlockSlots, EvictableTx as _, PowClaimEvictionSettings},
    },
    metrics,
    storage::{BlockSlotLookup, MempoolStorageAdapter},
};

const REMOVED_ITEM_GRACE_PERIOD: Duration = Duration::from_mins(10);
const LOG_TARGET: &str = mempool::POOL;

/// Default time a pending transaction may stay in the pool before eviction.
pub const DEFAULT_TX_TTL: Duration = Duration::from_hours(1);

/// Settings for the [`Mempool`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MempoolSettings {
    /// How long a pending transaction is allowed to stay in the pool before
    /// it is evicted. `None` disables expiry-based eviction.
    pub tx_ttl: Option<Duration>,
    /// Eviction of transactions carrying an expired `PoW` reward claim, with
    /// the settings of every era. `None` disables it.
    pub pow_claim: Option<EraSchedule<PowClaimEvictionSettings>>,
}

impl Default for MempoolSettings {
    fn default() -> Self {
        Self {
            tx_ttl: Some(DEFAULT_TX_TTL),
            pow_claim: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PoolRecoveryState<Key>
where
    Key: Hash + Eq + Ord,
{
    /// Pending item keys mapped to their admission timestamp in milliseconds,
    /// oldest first.
    pub pending_items: IndexMap<Key, AdmissionTime>,
    /// Retired item keys not carried by a canonical block, i.e. evicted.
    pub removed_items: BTreeMap<Key, RemovedItem>,
    /// Retired item keys carried by a canonical block that is not yet
    /// immutable.
    #[serde(default)]
    pub included_in_canonical: HashMap<Key, IncludedItem>,
    pub last_item_timestamp: u64,
}

/// A retired item not carried by a canonical block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovedItem {
    /// Retirement timestamp in milliseconds.
    pub removed_at: u64,
    /// Admission timestamp in milliseconds, kept if the item is admitted
    /// again.
    pub admitted_at: AdmissionTime,
}

/// A retired item carried by a canonical block that is not yet immutable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncludedItem {
    /// Slot of the block carrying the item.
    pub slot: Slot,
    /// Admission timestamp in milliseconds, kept if the item is admitted
    /// again after a reorg.
    pub admitted_at: AdmissionTime,
}

/// Transactions are keyed by their hash.
pub type ItemKey<Item> = <Item as Hashable>::Hash;

pub struct Mempool<BlockId, Item, Storage, RuntimeServiceId>
where
    Item: Hashable<Hash: PrefixedKey>,
{
    /// Pending items mapped to their admission timestamp in milliseconds,
    /// oldest first.
    pending_items: IndexMap<ItemKey<Item>, AdmissionTime>,
    /// Secondary index over [`Self::pending_items`],
    /// [`Self::included_in_canonical`] and [`Self::removed_items`], keyed
    /// by the hash prefix a block proposal would use to refer to a
    /// transaction. Retired transactions stay indexed until their bodies
    /// are deleted, so a competing proposal that references one still
    /// resolves.
    ///
    /// Buckets are plain vectors: a 128-bit prefix cannot be made to collide at
    /// any feasible cost, so every one holds a single key in practice. A set
    /// would cost a hash table per bucket to deduplicate keys that are already
    /// unique.
    by_prefix: HashMap<<ItemKey<Item> as PrefixedKey>::Prefix, Vec<ItemKey<Item>>>,
    /// Retired items not carried by a canonical block, i.e. evicted. They
    /// keep their prefix index entry and body for a grace period after
    /// retirement, unlike [`Self::included_in_canonical`], which
    /// keeps them until the carrying block becomes immutable.
    removed_items: BTreeMap<ItemKey<Item>, RemovedItem>,
    /// Retired items carried by a canonical block that is not yet immutable.
    /// They keep their prefix index entry and body until the block becomes
    /// immutable, since until then a competing block can carry them too.
    included_in_canonical: HashMap<ItemKey<Item>, IncludedItem>,
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
            .field("included_in_canonical", &self.included_in_canonical)
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

    fn new(settings: Self::Settings, _: Era, storage: Self::Storage) -> Self {
        Self {
            pending_items: IndexMap::new(),
            by_prefix: HashMap::new(),
            removed_items: BTreeMap::new(),
            included_in_canonical: HashMap::new(),
            last_item_timestamp: 0,
            evictor: Evictor::new(&settings),
            storage_adapter: storage,
            _phantom: std::marker::PhantomData,
        }
    }

    async fn rotate_era(&mut self, era: Era) {
        self.storage_adapter.rotate_era(era).await;
    }

    async fn complete_era_transition(&mut self, ended_era: Era) {
        self.storage_adapter
            .complete_era_transition(ended_era)
            .await;
    }

    async fn add_item<I: Into<Self::Item> + Send>(
        &mut self,
        key: Self::Key,
        item: I,
    ) -> Result<(), MempoolError> {
        self.prune_removed_items().await;

        if self.pending_items.contains_key(&key) || self.included_in_canonical.contains_key(&key) {
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

        // If `item` is admitted again after being retired, remove it from
        // `removed_items` and use the original admission time stored there.
        let removed_item = self.removed_items.remove(&key);
        let admitted_at = removed_item.map_or(timestamp, |removed| removed.admitted_at);
        self.index_by_prefix(&key);
        self.evictor.on_add(&key, &item, timestamp, &block_slots);
        let index = self.insert_pending(key, admitted_at);
        self.last_item_timestamp = timestamp;
        tracing::debug!(
            target: LOG_TARGET,
            admitted_at,
            index,
            readmission = removed_item.is_some(),
            pending_items = self.pending_items.len(),
            last_item_timestamp = self.last_item_timestamp,
            "Added item to mempool",
        );

        metrics::mempool_transactions_added();
        metrics::mempool_transactions_pending(self.pending_items.len());

        Ok(())
    }

    async fn view(
        &self,
        _ancestor_hint: BlockId,
    ) -> Result<Pin<Box<dyn Stream<Item = Self::Item> + Send>>, MempoolError> {
        self.get_items_by_keys(self.pending_items.keys().copied())
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

    async fn on_canonical_chain_updated(
        &mut self,
        entered: HashMap<Self::Key, Slot>,
        reorged: Vec<Self::Item>,
    ) -> Vec<Self::Item> {
        // A reorged tx leaves `included_in_canonical` to be admitted again
        // below. If its admission fails, it is retained in `removed_items`
        // like any other retired tx.
        let now = current_timestamp_millis();
        for item in &reorged {
            let key = item.hash();
            if let Some(included) = self.included_in_canonical.remove(&key) {
                self.removed_items.insert(
                    key,
                    RemovedItem {
                        removed_at: now,
                        admitted_at: included.admitted_at,
                    },
                );
            }
        }

        let mut count = 0usize;
        for (key, slot) in entered {
            let admitted_at = if let Some(admitted_at) = self.pending_items.shift_remove(&key) {
                self.evictor.on_remove(&key);
                count += 1;
                admitted_at
            } else if let Some(removed) = self.removed_items.remove(&key) {
                removed.admitted_at
            } else if let Some(included) = self.included_in_canonical.get(&key) {
                included.admitted_at
            } else {
                // The mempool never held the tx, so there is no body to retain.
                continue;
            };
            self.included_in_canonical
                .insert(key, IncludedItem { slot, admitted_at });
        }

        log_retired_items(count, "included", self.pending_items.len());
        metrics::mempool_transactions_removed(count);
        metrics::mempool_transactions_pending(self.pending_items.len());

        let mut readmitted = Vec::with_capacity(reorged.len());
        for item in reorged {
            match self.add_item(item.hash(), item.clone()).await {
                Ok(()) => readmitted.push(item),
                // Already pending: the pool didn't hold it when its block
                // entered the canonical chain, and received it afterwards.
                Err(MempoolError::ExistingItem) => {}
                Err(error) => tracing::debug!(
                    target: LOG_TARGET,
                    "Could not admit a reorged item again: {error}"
                ),
            }
        }
        readmitted
    }

    async fn on_processed_block(&mut self, block_id: BlockId, slot: Slot, lib_slot: Slot) {
        self.prune_removed_items().await;
        self.release_immutable(lib_slot).await;

        self.evictor.on_processed_block(&block_id, slot);

        let now = current_timestamp_millis();
        self.evict(self.evictor.select_evictions(now, &self.pending_items), now);

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
                if self.pending_items.contains_key(key) {
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
            pending_items: self.pending_items.clone(),
            removed_items: self.removed_items.clone(),
            included_in_canonical: self.included_in_canonical.clone(),
            last_item_timestamp: self.last_item_timestamp,
        }
    }

    fn recover(
        settings: <Self as MemPool>::Settings,
        _: Era,
        state: Self::RecoveryState,
        storage: <Self as MemPool>::Storage,
    ) -> Self {
        // `by_prefix` is derived, so it is rebuilt rather than restored.
        let mut by_prefix: HashMap<<ItemKey<Item> as PrefixedKey>::Prefix, Vec<ItemKey<Item>>> =
            HashMap::with_capacity(
                state.pending_items.len()
                    + state.removed_items.len()
                    + state.included_in_canonical.len(),
            );
        for key in state
            .pending_items
            .keys()
            .chain(state.removed_items.keys())
            .chain(state.included_in_canonical.keys())
        {
            let bucket = by_prefix.entry(key.key_prefix()).or_default();
            if !bucket.contains(key) {
                bucket.push(*key);
            }
        }

        Self {
            pending_items: state.pending_items,
            by_prefix,
            removed_items: state.removed_items,
            included_in_canonical: state.included_in_canonical,
            last_item_timestamp: state.last_item_timestamp,
            evictor: Evictor::new(&settings),
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
    /// would use to refer to it, unless it is already there.
    ///
    /// A retired transaction stays indexed during its grace period, so it is
    /// still there if it is admitted again within that period. Indexing it a
    /// second time would make the lookup return two keys, and resolution
    /// treats that as a non-unique reference.
    fn index_by_prefix(&mut self, key: &ItemKey<Item>) {
        let bucket = self.by_prefix.entry(key.key_prefix()).or_default();
        if !bucket.contains(key) {
            bucket.push(*key);
        }
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

    /// Retire pending items the evictor selected, keeping it in sync. Items
    /// carried by a canonical block go through `on_canonical_chain_updated`
    /// instead.
    ///
    /// A retired item keeps its prefix index entry, and its body, until
    /// [`Self::prune_removed_items`] drops both after the grace period.
    ///
    /// Items not in `pending_items` are ignored, such as ones the mempool
    /// doesn't hold (e.g. from a downloaded block).
    fn evict(&mut self, keys: impl IntoIterator<Item = ItemKey<Item>>, at: u64) {
        let mut count = 0usize;
        for key in keys {
            let Some(admitted_at) = self.pending_items.shift_remove(&key) else {
                continue;
            };
            self.evictor.on_remove(&key);
            self.removed_items.insert(
                key,
                RemovedItem {
                    removed_at: at,
                    admitted_at,
                },
            );
            count += 1;
        }

        log_retired_items(count, "evicted", self.pending_items.len());
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
            .filter(|(_, removed)| now.saturating_sub(removed.removed_at) >= grace_period_millis)
            .map(|(key, _)| *key)
            .collect::<Vec<_>>();

        if !self.delete_retained(&expired_keys).await {
            return;
        }
        for key in expired_keys {
            self.removed_items.remove(&key);
        }
    }

    /// Delete the bodies of retired `keys` and drop them from the prefix
    /// index. Returns `false`, leaving everything in place, if storage fails.
    async fn delete_retained(&mut self, keys: &[ItemKey<Item>]) -> bool {
        if keys.is_empty() {
            return true;
        }
        if let Err(e) = self.storage_adapter.remove_items(keys).await {
            tracing::warn!(target: LOG_TARGET, "Failed to delete retired items from storage: {e:?}");
            return false;
        }
        for key in keys {
            self.unindex_by_prefix(key);
        }
        true
    }

    /// Insert `key` into [`Self::pending_items`] at the position its
    /// admission timestamp gives it. A new item normally goes last; an item
    /// admitted again goes back among the items admitted around the same
    /// time. Returns the position.
    fn insert_pending(&mut self, key: ItemKey<Item>, admitted_at: AdmissionTime) -> usize {
        let index = self
            .pending_items
            .partition_point(|_, timestamp| *timestamp <= admitted_at);
        self.pending_items.shift_insert(index, key, admitted_at);
        index
    }

    /// Release the items carried by blocks that became immutable, since no
    /// competing block can carry them anymore.
    async fn release_immutable(&mut self, lib_slot: Slot) {
        let immutable_keys = self
            .included_in_canonical
            .iter()
            .filter(|(_, included)| included.slot <= lib_slot)
            .map(|(key, _)| *key)
            .collect::<Vec<_>>();

        if !self.delete_retained(&immutable_keys).await {
            return;
        }
        for key in immutable_keys {
            self.included_in_canonical.remove(&key);
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
