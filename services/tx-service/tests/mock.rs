use std::{
    collections::{BTreeMap, HashMap, HashSet},
    num::{NonZero, NonZeroU64},
    pin::Pin,
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::Duration,
};

use async_trait::async_trait;
use futures::{Stream, StreamExt as _, stream};
use indexmap::IndexMap;
use lb_binary_codec::bincode::{DeserializeOp as _, SerializeOp as _};
use lb_core::{
    block::MAX_BLOCK_TRANSACTIONS_SIZE,
    header::HeaderId,
    mantle::{
        Op,
        ops::{
            channel::inscribe::{Inscription, InscriptionOp, MAX_BYTES as MAX_INSCRIPTION_BYTES},
            pow::ClaimPowRewardOp,
        },
        traits::Hashable as _,
        transactions::{
            hash::{PrefixedKey as _, TxHash, TxHashPrefix},
            tx_list::ops::Ops,
        },
    },
};
use lb_cryptarchia_engine::Slot;
use lb_network_service::{
    NetworkService,
    backends::mock::{Mock, MockBackendMessage, MockConfig, MockMessage},
    config::NetworkConfig,
    message::NetworkMsg,
};
use lb_services_utils::overwatch::{
    RecoveryData, recovery::operators::RecoveryBackend as RecoveryBackendTrait,
};
use lb_storage_service::{
    StorageService,
    recovery::{StorageRecoveryBackend, load_recovery_data},
    rocksdb,
};
use lb_time::{
    Era,
    era::{EraEntriesAfterGenesis, EraSchedule, EraTiming},
};
use lb_time_service::{
    TimeService, TimeServiceEraSettings, TimeServiceSettings, backends::SystemTimeBackend,
};
use lb_tracing_service::{Tracing, TracingSettings};
use lb_utils::noop_service::NoService;
use logos_blockchain_tx_service::{
    MempoolMsg, TxMempoolSettings,
    backend::{
        MemPool as _, Mempool, MempoolError, MempoolSettings, PoolRecoveryState,
        PowClaimEvictionSettings, RecoverableMempool as _, Status,
    },
    network::adapters::mock::{MOCK_TX_CONTENT_TOPIC, MockAdapter, mock_message},
    storage::{BlockSlotLookup, MempoolStorageAdapter, adapters::rocksdb::RocksStorageAdapter},
    tx::{service::GenericTxMempoolService, state::TxMempoolState},
};
use overwatch::{
    overwatch::{OverwatchRunner, Shutdown},
    services::{ServiceData, relay::OutboundRelay},
};
use overwatch_derive::*;
use tempfile::TempDir;
use time::OffsetDateTime;

type MockRecoveryBackend = StorageRecoveryBackend<
    TxMempoolState<PoolRecoveryState<TxHash>, MempoolSettings, ()>,
    TxMempoolSettings<MempoolSettings, ()>,
    RuntimeServiceId,
>;

type MockMempoolService = GenericTxMempoolService<
    Mempool<HeaderId, Ops, RocksStorageAdapter<Ops, TxHash>, RuntimeServiceId>,
    MockAdapter<RuntimeServiceId>,
    MockRecoveryBackend,
    RocksStorageAdapter<Ops, TxHash>,
    SystemTimeBackend,
    RuntimeServiceId,
>;

/// A pool over the in-memory storage adapter, for tests that drive the pool
/// directly.
type InMemoryPool = Mempool<HeaderId, Ops, InMemoryStorageAdapter, RuntimeServiceId>;

#[derive_services(panic_policy = Shutdown)]
struct MockPoolNode {
    logging: Tracing<RuntimeServiceId>,
    network: NetworkService<Mock, SystemTimeBackend, RuntimeServiceId>,
    storage: StorageService<RuntimeServiceId>,
    mockpool: MockMempoolService,
    time: TimeService<SystemTimeBackend, RuntimeServiceId>,
    no_service: NoService,
}

/// A transaction whose only op inscribes `payload`, so distinct payloads
/// give distinct transactions.
fn tx(payload: &str) -> Ops {
    inscribing(payload.as_bytes())
}

fn inscribing(bytes: &[u8]) -> Ops {
    Ops::from([Op::ChannelInscribe(InscriptionOp {
        inscription: Inscription::try_from_iter(bytes.iter().copied())
            .expect("inscription should fit"),
        ..InscriptionOp::sample()
    })])
}

/// A transaction claiming the `PoW` reward for a puzzle solved against
/// `anchor`.
fn claim_tx(anchor: HeaderId) -> Ops {
    Ops::from([Op::ClaimPowReward(ClaimPowRewardOp {
        block_hash: anchor.into(),
        ..ClaimPowRewardOp::sample()
    })])
}

fn sample_removed_tx() -> Ops {
    tx("removed-but-fetchable")
}

fn mock_pool_node_settings(
    predefined_messages: Vec<MockMessage>,
) -> (MockPoolNodeServiceSettings, TempDir) {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let db_path = temp_dir.path().join("test_db");

    (
        MockPoolNodeServiceSettings {
            network: NetworkConfig {
                backend: MockConfig {
                    predefined_messages,
                    duration: Duration::from_millis(100),
                    seed: 0,
                    version: 1,
                    weights: None,
                },
            },
            storage: rocksdb::RocksBackendSettings {
                db_path,
                read_only: false,
                column_family: None,
            },
            mockpool: TxMempoolSettings {
                pool: MempoolSettings::default(),
                network_adapter: (),
                seen_items_subscription_channel_size: 64.try_into().unwrap(),
                recovery_data: RecoveryData::default(),
            },
            logging: TracingSettings::default(),
            // The network service and the mempool follow the eras of the clock.
            time: TimeServiceSettings {
                backend: (),
                era_schedule: era_schedule()
                    // A single era: no transition period ever opens.
                    .map(|era| TimeServiceEraSettings {
                        timing: *era.parameters,
                        transition_slots: 0,
                    }),
            },
            no_service: (),
        },
        temp_dir,
    )
}

fn run_with_mock_pool_node(
    predefined_messages: Vec<MockMessage>,
    run: impl FnOnce(MockPoolNodeServiceSettings, TempDir),
) {
    let (settings, temp_dir) = mock_pool_node_settings(predefined_messages);
    run(settings, temp_dir);
}

async fn add_tx(
    mempool_outbound: &OutboundRelay<<MockMempoolService as ServiceData>::Message>,
    tx: Ops,
) -> Result<(), MempoolError> {
    let (reply_channel, reply) = tokio::sync::oneshot::channel();
    mempool_outbound
        .send(MempoolMsg::Add {
            key: tx.hash(),
            payload: tx,
            reply_channel,
        })
        .await
        .unwrap();

    reply.await.expect("mempool should reply to local add")
}

async fn pending_txs(
    mempool_outbound: &OutboundRelay<<MockMempoolService as ServiceData>::Message>,
) -> Vec<Ops> {
    let (reply_channel, reply) = tokio::sync::oneshot::channel();
    mempool_outbound
        .send(MempoolMsg::View {
            ancestor_hint: [0; 32].into(),
            reply_channel,
        })
        .await
        .unwrap();

    reply
        .await
        .expect("mempool should reply to view")
        .collect::<Vec<_>>()
        .await
}

async fn txs_by_prefix(
    mempool_outbound: &OutboundRelay<<MockMempoolService as ServiceData>::Message>,
    prefix: TxHashPrefix,
) -> Vec<Ops> {
    let (reply_channel, reply) = tokio::sync::oneshot::channel();
    mempool_outbound
        .send(MempoolMsg::GetTransactionsByPrefix {
            prefix,
            reply_channel,
        })
        .await
        .unwrap();

    reply
        .await
        .expect("mempool should reply to prefix lookup")
        .expect("prefix lookup should succeed")
        .collect::<Vec<_>>()
        .await
}

#[derive(Clone, Default)]
struct InMemoryStorageAdapter {
    items: Arc<Mutex<BTreeMap<TxHash, Ops>>>,
    /// Slots of the blocks "stored" alongside the transactions.
    block_slots: Arc<Mutex<HashMap<HeaderId, Slot>>>,
    /// Blocks whose lookup fails instead of answering.
    failing_blocks: Arc<Mutex<HashSet<HeaderId>>>,
}

impl InMemoryStorageAdapter {
    fn with_block(self, block_id: HeaderId, slot: u64) -> Self {
        self.block_slots
            .lock()
            .expect("in-memory storage adapter lock should not be poisoned")
            .insert(block_id, Slot::from(slot));
        self
    }

    fn with_failing_block(self, block_id: HeaderId) -> Self {
        self.failing_blocks
            .lock()
            .expect("in-memory storage adapter lock should not be poisoned")
            .insert(block_id);
        self
    }
}

#[derive(Clone, Default)]
struct FailingStorageAdapter;

#[async_trait]
impl MempoolStorageAdapter<RuntimeServiceId> for InMemoryStorageAdapter {
    type Item = Ops;
    type Key = TxHash;
    type BlockId = HeaderId;
    type Error = MempoolError;

    fn new(
        _: Era,
        _storage_relay: OutboundRelay<<StorageService<RuntimeServiceId> as ServiceData>::Message>,
    ) -> Self {
        Self::default()
    }

    async fn rotate_era(&mut self, _: Era) {}

    async fn complete_era_transition(&mut self, _ended_era: Era) {}

    async fn store_item(&mut self, key: Self::Key, item: Self::Item) -> Result<(), Self::Error> {
        self.items
            .lock()
            .expect("in-memory storage adapter lock should not be poisoned")
            .insert(key, item);
        Ok(())
    }

    async fn get_items(
        &self,
        keys: &[Self::Key],
    ) -> Result<Pin<Box<dyn Stream<Item = Self::Item> + Send>>, Self::Error> {
        let items = {
            let storage = self
                .items
                .lock()
                .expect("in-memory storage adapter lock should not be poisoned");
            keys.iter()
                .filter_map(|key| storage.get(key).cloned())
                .collect::<Vec<_>>()
        };

        Ok(Box::pin(stream::iter(items)))
    }

    async fn remove_items(&mut self, keys: &[Self::Key]) -> Result<(), Self::Error> {
        for key in keys {
            self.items
                .lock()
                .expect("in-memory storage adapter lock should not be poisoned")
                .remove(key);
        }
        Ok(())
    }

    async fn get_block_slots(
        &self,
        block_ids: &[Self::BlockId],
    ) -> BlockSlotLookup<Self::BlockId, Self::Error> {
        let known = self
            .block_slots
            .lock()
            .expect("in-memory storage adapter lock should not be poisoned")
            .clone();
        let failing = self
            .failing_blocks
            .lock()
            .expect("in-memory storage adapter lock should not be poisoned")
            .clone();
        let mut lookup = BlockSlotLookup::default();
        for block_id in block_ids {
            if failing.contains(block_id) {
                lookup.failed.push((
                    *block_id,
                    MempoolError::StorageError("test block lookup failure".to_owned()),
                ));
            } else if let Some(slot) = known.get(block_id) {
                lookup.slots.insert(*block_id, *slot);
            }
        }
        lookup
    }
}

#[async_trait]
impl MempoolStorageAdapter<RuntimeServiceId> for FailingStorageAdapter {
    type Item = Ops;
    type Key = TxHash;
    type BlockId = HeaderId;
    type Error = MempoolError;

    fn new(
        _: Era,
        _storage_relay: OutboundRelay<<StorageService<RuntimeServiceId> as ServiceData>::Message>,
    ) -> Self {
        Self
    }

    async fn rotate_era(&mut self, _: Era) {}

    async fn complete_era_transition(&mut self, _ended_era: Era) {}

    async fn store_item(&mut self, _key: Self::Key, _item: Self::Item) -> Result<(), Self::Error> {
        Err(MempoolError::StorageError(
            "test storage failure".to_owned(),
        ))
    }

    async fn get_items(
        &self,
        _keys: &[Self::Key],
    ) -> Result<Pin<Box<dyn Stream<Item = Self::Item> + Send>>, Self::Error> {
        Ok(Box::pin(stream::empty()))
    }

    async fn remove_items(&mut self, _keys: &[Self::Key]) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn get_block_slots(
        &self,
        block_ids: &[Self::BlockId],
    ) -> BlockSlotLookup<Self::BlockId, Self::Error> {
        BlockSlotLookup {
            slots: HashMap::new(),
            failed: block_ids
                .iter()
                .map(|block_id| {
                    (
                        *block_id,
                        MempoolError::StorageError("test storage failure".to_owned()),
                    )
                })
                .collect(),
        }
    }
}

#[test]
fn test_mock_pool_recovery_state() {
    let recovery_state = PoolRecoveryState::<TxHash> {
        pending_items: IndexMap::new(),
        removed_items: BTreeMap::new(),
        included_in_canonical: HashMap::new(),
        last_item_timestamp: 1_234_567_890,
    };

    let serialized = recovery_state.to_bytes().expect("Should serialize");

    let deserialized: PoolRecoveryState<TxHash> =
        PoolRecoveryState::from_bytes(&serialized).expect("Should deserialize");

    assert_eq!(deserialized.pending_items, recovery_state.pending_items);
    assert_eq!(deserialized.removed_items, recovery_state.removed_items);
    assert_eq!(
        deserialized.last_item_timestamp,
        recovery_state.last_item_timestamp
    );
}

const fn ttl_settings(tx_ttl: Duration) -> MempoolSettings {
    MempoolSettings {
        tx_ttl: Some(tx_ttl),
        pow_claim: None,
    }
}

/// The single era the tests run in, of 100-slot epochs.
fn era_schedule() -> EraSchedule<EraTiming> {
    EraSchedule::new(
        OffsetDateTime::UNIX_EPOCH,
        EraTiming {
            slot_duration: Duration::from_secs(1),
            epoch_length: NonZero::new(100).unwrap(),
        },
        EraEntriesAfterGenesis::empty(),
    )
    .unwrap()
}

const TEST_SLOT_WINDOW: u64 = 10;

/// Settings with only `PoW` claim eviction enabled, so the TTL cannot
/// interfere. With a window of 10 and the grace of twice that, a claim
/// anchored at slot 100 is accepted through slot 110 and evicted from 131.
fn pow_claim_settings() -> MempoolSettings {
    MempoolSettings {
        tx_ttl: None,
        pow_claim: Some(era_schedule().map(|_| PowClaimEvictionSettings {
            slot_window: NonZeroU64::new(TEST_SLOT_WINDOW).expect("non-zero"),
        })),
    }
}

const TEST_TX_TTL: Duration = Duration::from_hours(1);

/// Build a pool holding one pending tx whose insertion timestamp is backdated
/// to the unix epoch, so it is expired for any reasonable TTL.
async fn pool_with_backdated_tx(settings: MempoolSettings) -> (InMemoryPool, Ops) {
    let storage = InMemoryStorageAdapter::default();
    let mut pool = InMemoryPool::new(settings.clone(), Era::genesis(), storage.clone());

    let tx = sample_removed_tx();
    let tx_id = tx.hash();

    pool.add_item(tx_id, tx.clone())
        .await
        .expect("tx should be added");

    let mut saved_state = pool.save();
    saved_state.pending_items.insert(tx_id, 0);

    (
        Mempool::recover(settings, Era::genesis(), saved_state, storage),
        tx,
    )
}

#[tokio::test]
async fn expired_tx_is_evicted_on_next_sweep() {
    let storage = InMemoryStorageAdapter::default();
    let mut pool = InMemoryPool::new(ttl_settings(TEST_TX_TTL), Era::genesis(), storage.clone());

    let old_tx = sample_removed_tx();
    let old_tx_id = old_tx.hash();
    let old_tx_prefix = old_tx_id.key_prefix();

    let fresh_tx = tx("fresh");
    let fresh_tx_id = fresh_tx.hash();

    pool.add_item(old_tx_id, old_tx.clone())
        .await
        .expect("old tx should be added");
    pool.add_item(fresh_tx_id, fresh_tx.clone())
        .await
        .expect("fresh tx should be added");

    let mut saved_state = pool.save();
    saved_state.pending_items.insert(old_tx_id, 0);
    let mut pool = InMemoryPool::recover(
        ttl_settings(TEST_TX_TTL),
        Era::genesis(),
        saved_state,
        storage,
    );

    assert_eq!(pool.pending_item_count(), 2);
    assert_eq!(pool.status(&[old_tx_id]), vec![Status::Pending]);
    assert_eq!(
        pool.keys_by_prefix(&old_tx_prefix)
            .copied()
            .collect::<Vec<_>>(),
        vec![old_tx_id]
    );

    pool.remove(&[]).await;

    assert_eq!(pool.pending_item_count(), 1);
    assert_eq!(pool.status(&[old_tx_id]), vec![Status::Unknown]);
    assert_eq!(pool.status(&[fresh_tx_id]), vec![Status::Pending]);
    // An evicted tx still resolves during its grace period.
    assert_eq!(
        pool.keys_by_prefix(&old_tx_prefix)
            .copied()
            .collect::<Vec<_>>(),
        vec![old_tx_id]
    );

    let pending_after_eviction = pool
        .view([0; 32].into())
        .await
        .expect("pending view should load")
        .collect::<Vec<_>>()
        .await;
    assert_eq!(pending_after_eviction, vec![fresh_tx]);

    let fetched_after_eviction = pool
        .get_items_by_keys([old_tx_id])
        .await
        .expect("evicted tx should still be fetchable during grace period")
        .collect::<Vec<_>>()
        .await;
    assert_eq!(fetched_after_eviction, vec![old_tx]);
}

#[tokio::test]
async fn expired_tx_is_not_evicted_when_ttl_is_disabled() {
    let (mut pool, tx) = pool_with_backdated_tx(MempoolSettings {
        tx_ttl: None,
        pow_claim: None,
    })
    .await;
    let tx_id = tx.hash();

    for _ in 0..10 {
        pool.remove(&[]).await;
    }

    assert_eq!(pool.pending_item_count(), 1);
    assert_eq!(pool.status(&[tx_id]), vec![Status::Pending]);
}

#[tokio::test]
async fn storage_failure_does_not_mark_tx_pending() {
    let mut pool = Mempool::<HeaderId, Ops, FailingStorageAdapter, RuntimeServiceId>::new(
        MempoolSettings::default(),
        Era::genesis(),
        FailingStorageAdapter,
    );

    let tx = sample_removed_tx();
    let tx_id = tx.hash();

    let error = pool
        .add_item(tx_id, tx)
        .await
        .expect_err("storage failure should reject mempool add");

    assert!(matches!(error, MempoolError::StorageError(_)));
    assert_eq!(pool.pending_item_count(), 0);
    assert_eq!(pool.status(&[tx_id]), vec![Status::Unknown]);
    assert!(pool.save().pending_items.is_empty());
}

#[tokio::test]
async fn removed_items_are_not_pending_but_still_fetchable() {
    let storage = InMemoryStorageAdapter::default();
    let mut pool = InMemoryPool::new(MempoolSettings::default(), Era::genesis(), storage.clone());

    let tx = sample_removed_tx();
    let tx_id = tx.hash();

    pool.add_item(tx_id, tx.clone())
        .await
        .expect("tx should be added");

    assert_eq!(pool.pending_item_count(), 1);
    assert_eq!(pool.status(&[tx_id]), vec![Status::Pending]);

    let pending_before_remove = pool
        .view([0; 32].into())
        .await
        .expect("pending view should load")
        .collect::<Vec<_>>()
        .await;

    assert_eq!(pending_before_remove, vec![tx.clone()]);

    pool.remove(&[tx_id]).await;

    assert_eq!(pool.pending_item_count(), 0);
    assert_eq!(pool.status(&[tx_id]), vec![Status::Unknown]);

    let pending_after_remove = pool
        .view([0; 32].into())
        .await
        .expect("pending view should still work")
        .collect::<Vec<_>>()
        .await;
    assert_eq!(pending_after_remove, []);

    let fetched_after_remove = pool
        .get_items_by_keys([tx_id])
        .await
        .expect("removed tx should still be fetchable")
        .collect::<Vec<_>>()
        .await;
    assert_eq!(fetched_after_remove, vec![tx.clone()]);
}

#[tokio::test]
async fn removed_items_remain_fetchable_after_recovery() {
    let storage = InMemoryStorageAdapter::default();
    let mut pool = InMemoryPool::new(MempoolSettings::default(), Era::genesis(), storage.clone());

    let tx = sample_removed_tx();
    let tx_id = tx.hash();

    pool.add_item(tx_id, tx.clone())
        .await
        .expect("tx should be added");
    pool.remove(&[tx_id]).await;

    let saved_state = pool.save();
    assert!(saved_state.pending_items.is_empty());
    assert!(saved_state.removed_items.contains_key(&tx_id));

    let recovered_pool = InMemoryPool::recover(
        MempoolSettings::default(),
        Era::genesis(),
        saved_state,
        storage,
    );

    assert_eq!(recovered_pool.pending_item_count(), 0);
    assert_eq!(recovered_pool.status(&[tx_id]), vec![Status::Unknown]);
    assert_eq!(
        recovered_pool
            .keys_by_prefix(&tx_id.key_prefix())
            .copied()
            .collect::<Vec<_>>(),
        vec![tx_id],
        "a removed tx must still resolve after recovery"
    );

    let fetched_after_recovery = recovered_pool
        .get_items_by_keys([tx_id])
        .await
        .expect("removed tx should still be fetchable after recovery")
        .collect::<Vec<_>>()
        .await;
    assert_eq!(fetched_after_recovery, vec![tx]);
}

/// A removed tx keeps resolving until its grace period ends, so a competing
/// proposal that references it can still be reconstructed.
#[tokio::test]
async fn removed_tx_resolves_until_its_grace_period_ends() {
    let storage = InMemoryStorageAdapter::default();
    let mut pool = InMemoryPool::new(MempoolSettings::default(), Era::genesis(), storage.clone());

    let tx = sample_removed_tx();
    let tx_id = tx.hash();
    pool.add_item(tx_id, tx.clone())
        .await
        .expect("tx should be added");
    pool.remove(&[tx_id]).await;

    assert_eq!(
        pool.keys_by_prefix(&tx_id.key_prefix())
            .copied()
            .collect::<Vec<_>>(),
        vec![tx_id]
    );

    // Backdate the removal past the grace period, and trigger the pruning.
    let mut saved_state = pool.save();
    saved_state.removed_items.insert(tx_id, 0); // set the retirement time to 0 (1970)
    let mut pool = InMemoryPool::recover(
        MempoolSettings::default(),
        Era::genesis(),
        saved_state,
        storage,
    );
    // Trigger pruning removed items by calling `remove`
    pool.remove(&[]).await;

    // Check that the tx item (including prefix) has been pruned
    assert!(pool.keys_by_prefix(&tx_id.key_prefix()).next().is_none());
    assert!(!pool.save().removed_items.contains_key(&tx_id));
    assert_eq!(
        pool.get_items_by_keys([tx_id])
            .await
            .expect("storage lookup should succeed")
            .collect::<Vec<_>>()
            .await,
        vec![]
    );
}

/// A removed tx admitted again within its grace period is indexed once, so
/// its references stay unique.
#[tokio::test]
async fn readmitted_removed_tx_is_indexed_once() {
    let mut pool = InMemoryPool::new(
        MempoolSettings::default(),
        Era::genesis(),
        InMemoryStorageAdapter::default(),
    );

    let tx = sample_removed_tx();
    let tx_id = tx.hash();
    pool.add_item(tx_id, tx.clone())
        .await
        .expect("tx should be added");
    pool.remove(&[tx_id]).await;
    pool.add_item(tx_id, tx)
        .await
        .expect("a removed tx should be admitted again");

    assert_eq!(pool.status(&[tx_id]), vec![Status::Pending]);
    assert_eq!(
        pool.keys_by_prefix(&tx_id.key_prefix())
            .copied()
            .collect::<Vec<_>>(),
        vec![tx_id]
    );
}

/// Removing a hash the pool does not hold, such as a transaction of a
/// downloaded block, records nothing: there is no body to keep or prune.
#[tokio::test]
async fn removing_a_tx_the_pool_never_held_records_nothing() {
    let mut pool = InMemoryPool::new(
        MempoolSettings::default(),
        Era::genesis(),
        InMemoryStorageAdapter::default(),
    );

    let held = tx("held");
    let held_id = held.hash();
    pool.add_item(held_id, held)
        .await
        .expect("tx should be added");
    let unknown_id = tx("unknown").hash();

    pool.remove(&[unknown_id]).await;

    assert_eq!(pool.status(&[held_id]), vec![Status::Pending]);
    assert!(!pool.save().removed_items.contains_key(&unknown_id));
    assert!(
        pool.keys_by_prefix(&unknown_id.key_prefix())
            .next()
            .is_none()
    );
}

fn keys_with_prefix(pool: &InMemoryPool, key: TxHash) -> Vec<TxHash> {
    pool.keys_by_prefix(&key.key_prefix()).copied().collect()
}

async fn stored_items(pool: &InMemoryPool, key: TxHash) -> Vec<Ops> {
    pool.get_items_by_keys([key])
        .await
        .expect("storage lookup should succeed")
        .collect()
        .await
}

/// A tx carried by a canonical block stays resolvable until the block becomes
/// immutable, so a competing proposal that references it still reconstructs.
#[tokio::test]
async fn included_tx_resolves_until_its_block_is_immutable() {
    let mut pool = InMemoryPool::new(
        MempoolSettings::default(),
        Era::genesis(),
        InMemoryStorageAdapter::default(),
    );
    let tx = tx("included");
    let tx_id = tx.hash();
    pool.add_item(tx_id, tx.clone())
        .await
        .expect("tx should be added");

    pool.on_canonical_chain_updated(HashMap::from([(tx_id, Slot::from(10u64))]), vec![])
        .await;

    assert_eq!(pool.status(&[tx_id]), vec![Status::Unknown]);
    assert!(pool.save().included_in_canonical.contains_key(&tx_id));
    assert_eq!(keys_with_prefix(&pool, tx_id), vec![tx_id]);

    // The block is not immutable yet.
    pool.on_processed_block(HeaderId::from([1; 32]), Slot::from(11u64), Slot::from(9u64))
        .await;
    assert_eq!(keys_with_prefix(&pool, tx_id), vec![tx_id]);
    assert_eq!(stored_items(&pool, tx_id).await, vec![tx]);

    // The block becomes immutable.
    pool.on_processed_block(
        HeaderId::from([2; 32]),
        Slot::from(12u64),
        Slot::from(10u64),
    )
    .await;
    assert_eq!(keys_with_prefix(&pool, tx_id), vec![]);
    assert_eq!(stored_items(&pool, tx_id).await, vec![]);
    assert!(!pool.save().included_in_canonical.contains_key(&tx_id));
}

/// A tx whose block left the canonical chain is admitted again.
#[tokio::test]
async fn reorged_tx_returns_to_pending() {
    let mut pool = InMemoryPool::new(
        MempoolSettings::default(),
        Era::genesis(),
        InMemoryStorageAdapter::default(),
    );
    let tx = tx("reorged");
    let tx_id = tx.hash();
    pool.add_item(tx_id, tx.clone())
        .await
        .expect("tx should be added");
    pool.on_canonical_chain_updated(HashMap::from([(tx_id, Slot::from(10u64))]), vec![])
        .await;

    let readmitted = pool
        .on_canonical_chain_updated(HashMap::new(), vec![tx.clone()])
        .await;

    assert_eq!(readmitted, vec![tx]);
    assert_eq!(pool.status(&[tx_id]), vec![Status::Pending]);
    assert!(!pool.save().included_in_canonical.contains_key(&tx_id));
    assert_eq!(keys_with_prefix(&pool, tx_id), vec![tx_id]);
}

/// A reorged tx that is already pending stays pending, and is not returned as
/// admitted again, so it isn't broadcast twice.
#[tokio::test]
async fn reorged_tx_already_pending_is_not_readmitted() {
    let mut pool = InMemoryPool::new(
        MempoolSettings::default(),
        Era::genesis(),
        InMemoryStorageAdapter::default(),
    );
    let tx = tx("reorged");
    let tx_id = tx.hash();
    // The pool didn't hold the tx when its block entered the canonical chain.
    pool.on_canonical_chain_updated(HashMap::from([(tx_id, Slot::from(10u64))]), vec![])
        .await;
    pool.add_item(tx_id, tx.clone())
        .await
        .expect("tx should be added");

    let readmitted = pool
        .on_canonical_chain_updated(HashMap::new(), vec![tx])
        .await;

    assert_eq!(readmitted, vec![]);
    assert_eq!(pool.status(&[tx_id]), vec![Status::Pending]);
    assert!(!pool.save().removed_items.contains_key(&tx_id));
}

/// A reorged tx that is not admitted again, here a claim that expired while
/// its block was canonical, is retained in `removed_items`, so it stays
/// resolvable for the grace period.
#[tokio::test]
async fn reorged_tx_not_admitted_again_moves_to_removed_items() {
    let anchor = HeaderId::from([1u8; 32]);
    let storage = InMemoryStorageAdapter::default().with_block(anchor, 100);
    let mut pool = InMemoryPool::new(pow_claim_settings(), Era::genesis(), storage);
    let claim = claim_tx(anchor);
    let claim_id = claim.hash();
    pool.add_item(claim_id, claim.clone())
        .await
        .expect("claim tx should be added");
    pool.on_canonical_chain_updated(HashMap::from([(claim_id, Slot::from(105u64))]), vec![])
        .await;
    // The claim's deadline is slot 131.
    processed_block(&mut pool, 131).await;

    let readmitted = pool
        .on_canonical_chain_updated(HashMap::new(), vec![claim])
        .await;

    assert_eq!(readmitted, vec![]);
    let state = pool.save();
    assert!(!state.included_in_canonical.contains_key(&claim_id));
    assert!(state.removed_items.contains_key(&claim_id));
    assert_eq!(keys_with_prefix(&pool, claim_id), vec![claim_id]);
}

/// A tx carried by a canonical block is a duplicate at admission, so gossiping
/// it again doesn't return it to pending.
#[tokio::test]
async fn included_tx_is_a_duplicate_at_admission() {
    let mut pool = InMemoryPool::new(
        MempoolSettings::default(),
        Era::genesis(),
        InMemoryStorageAdapter::default(),
    );
    let tx = tx("included");
    let tx_id = tx.hash();
    pool.add_item(tx_id, tx.clone())
        .await
        .expect("tx should be added");
    pool.on_canonical_chain_updated(HashMap::from([(tx_id, Slot::from(10u64))]), vec![])
        .await;

    let error = pool
        .add_item(tx_id, tx)
        .await
        .expect_err("an included tx must be a duplicate");

    assert!(matches!(error, MempoolError::ExistingItem));
    assert_eq!(pool.status(&[tx_id]), vec![Status::Unknown]);
    assert!(pool.save().included_in_canonical.contains_key(&tx_id));
}

/// A tx retired earlier, e.g. as inapplicable, moves to `included_in_canonical`
/// once a canonical block carries it.
#[tokio::test]
async fn removed_tx_entering_the_canonical_chain_moves_to_included() {
    let mut pool = InMemoryPool::new(
        MempoolSettings::default(),
        Era::genesis(),
        InMemoryStorageAdapter::default(),
    );
    let tx = tx("removed then included");
    let tx_id = tx.hash();
    pool.add_item(tx_id, tx).await.expect("tx should be added");
    pool.remove(&[tx_id]).await;

    pool.on_canonical_chain_updated(HashMap::from([(tx_id, Slot::from(10u64))]), vec![])
        .await;

    let state = pool.save();
    assert!(!state.removed_items.contains_key(&tx_id));
    assert_eq!(
        state.included_in_canonical.get(&tx_id),
        Some(&Slot::from(10u64))
    );
    assert_eq!(keys_with_prefix(&pool, tx_id), vec![tx_id]);
}

/// A tx carried by both a displaced block and an adopted one takes the slot of
/// the adopted block, so it is released when that block becomes immutable.
#[tokio::test]
async fn tx_carried_by_both_branches_takes_the_adopted_block_slot() {
    let mut pool = InMemoryPool::new(
        MempoolSettings::default(),
        Era::genesis(),
        InMemoryStorageAdapter::default(),
    );
    let tx = tx("carried by both branches");
    let tx_id = tx.hash();
    pool.add_item(tx_id, tx).await.expect("tx should be added");
    pool.on_canonical_chain_updated(HashMap::from([(tx_id, Slot::from(10u64))]), vec![])
        .await;

    pool.on_canonical_chain_updated(HashMap::from([(tx_id, Slot::from(12u64))]), vec![])
        .await;

    assert_eq!(
        pool.save().included_in_canonical.get(&tx_id),
        Some(&Slot::from(12u64))
    );
}

/// A tx the pool never held, e.g. from a downloaded block, has no body to
/// retain.
#[tokio::test]
async fn never_held_tx_entering_the_canonical_chain_is_not_retained() {
    let mut pool = InMemoryPool::new(
        MempoolSettings::default(),
        Era::genesis(),
        InMemoryStorageAdapter::default(),
    );
    let tx_id = tx("unknown").hash();

    pool.on_canonical_chain_updated(HashMap::from([(tx_id, Slot::from(10u64))]), vec![])
        .await;

    assert!(pool.save().included_in_canonical.is_empty());
    assert_eq!(keys_with_prefix(&pool, tx_id), vec![]);
}

/// A restart keeps the included txs and their prefix index entries.
#[tokio::test]
async fn included_tx_remains_resolvable_after_recovery() {
    let storage = InMemoryStorageAdapter::default();
    let mut pool = InMemoryPool::new(MempoolSettings::default(), Era::genesis(), storage.clone());
    let tx = tx("included");
    let tx_id = tx.hash();
    pool.add_item(tx_id, tx.clone())
        .await
        .expect("tx should be added");
    pool.on_canonical_chain_updated(HashMap::from([(tx_id, Slot::from(10u64))]), vec![])
        .await;

    let pool = InMemoryPool::recover(
        MempoolSettings::default(),
        Era::genesis(),
        pool.save(),
        storage,
    );

    assert!(pool.save().included_in_canonical.contains_key(&tx_id));
    assert_eq!(keys_with_prefix(&pool, tx_id), vec![tx_id]);
    assert_eq!(stored_items(&pool, tx_id).await, vec![tx]);
}

#[test]
fn local_submission_rejects_oversized_tx() {
    run_with_mock_pool_node(Vec::new(), |settings, _temp_dir| {
        let app = OverwatchRunner::<MockPoolNode>::run(settings, None)
            .map_err(|e| eprintln!("Error encountered: {e}"))
            .unwrap();

        drop(
            app.runtime()
                .handle()
                .block_on(app.handle().start_all_services()),
        );

        let mempool_outbound = app
            .runtime()
            .handle()
            .block_on(async { app.handle().relay::<MockMempoolService>().await.unwrap() });

        // Two inscriptions at their own maximum add up past the block limit.
        let max_inscription = Op::ChannelInscribe(InscriptionOp {
            inscription: Inscription::try_from_iter(std::iter::repeat_n(
                b'x',
                MAX_INSCRIPTION_BYTES,
            ))
            .expect("a maximal inscription should fit"),
            ..InscriptionOp::sample()
        });
        let oversized_tx = Ops::from([max_inscription.clone(), max_inscription]);

        let error = app
            .runtime()
            .handle()
            .block_on(add_tx(&mempool_outbound, oversized_tx))
            .expect_err("oversized tx should be rejected");

        assert!(matches!(
            error,
            MempoolError::ItemTooLarge {
                size,
                max: MAX_BLOCK_TRANSACTIONS_SIZE,
            } if size > MAX_BLOCK_TRANSACTIONS_SIZE
        ));

        assert_eq!(
            app.runtime()
                .handle()
                .block_on(pending_txs(&mempool_outbound)),
            []
        );

        drop(app.runtime().handle().block_on(app.handle().shutdown()));
        app.blocking_wait_finished()
            .expect("No service should panic");
    });
}

#[test]
fn mempool_view_preserves_receive_order() {
    run_with_mock_pool_node(Vec::new(), |settings, _temp_dir| {
        let app = OverwatchRunner::<MockPoolNode>::run(settings, None)
            .map_err(|e| eprintln!("Error encountered: {e}"))
            .unwrap();

        drop(
            app.runtime()
                .handle()
                .block_on(app.handle().start_all_services()),
        );

        let mempool_outbound = app
            .runtime()
            .handle()
            .block_on(async { app.handle().relay::<MockMempoolService>().await.unwrap() });

        let first_tx = tx("first");
        let second_tx = tx("second");

        let expected_txs = if first_tx.hash() < second_tx.hash() {
            vec![second_tx, first_tx]
        } else {
            vec![first_tx, second_tx]
        };

        for tx in &expected_txs {
            app.runtime()
                .handle()
                .block_on(add_tx(&mempool_outbound, tx.clone()))
                .expect("tx should be added");
        }

        let pending = app
            .runtime()
            .handle()
            .block_on(pending_txs(&mempool_outbound));

        assert_eq!(pending, expected_txs);

        drop(app.runtime().handle().block_on(app.handle().shutdown()));
        app.blocking_wait_finished()
            .expect("No service should panic");
    });
}

#[test]
fn test_mock_mempool() {
    let exist = Arc::new(AtomicBool::new(false));
    let exist2 = Arc::clone(&exist);

    let predefined_txs = [tx("This is foo"), tx("This is bar")];
    let predefined_messages = predefined_txs
        .iter()
        .map(|tx| mock_message(tx, 0))
        .collect::<Vec<_>>();

    run_with_mock_pool_node(predefined_messages, |settings, temp_dir| {
        let exp_txns: HashSet<TxHash> = predefined_txs.iter().map(Ops::hash).collect();
        let app = OverwatchRunner::<MockPoolNode>::run(settings, None)
            .map_err(|e| eprintln!("Error encountered: {e}"))
            .unwrap();
        let overwatch_handle = app.handle().clone();
        drop(
            app.runtime()
                .handle()
                .block_on(app.handle().start_all_services()),
        );

        app.spawn(async move {
            let network_outbound = overwatch_handle
                .relay::<NetworkService<_, _, _>>()
                .await
                .unwrap();
            let mempool_outbound = overwatch_handle
                .relay::<MockMempoolService>()
                .await
                .unwrap();

            // subscribe to the mock content topic
            network_outbound
                .send(NetworkMsg::Process(MockBackendMessage::RelaySubscribe {
                    topic: MOCK_TX_CONTENT_TOPIC.content_topic_name.to_string(),
                }))
                .await
                .unwrap();

            // try to wait all ops to be stored in mempool
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let (mtx, mrx) = tokio::sync::oneshot::channel();
                mempool_outbound
                    .send(MempoolMsg::View {
                        ancestor_hint: [0; 32].into(),
                        reply_channel: mtx,
                    })
                    .await
                    .unwrap();

                let items: HashSet<TxHash> = mrx.await.unwrap().map(|tx| tx.hash()).collect().await;

                if items.len() == exp_txns.len() {
                    assert_eq!(exp_txns, items);
                    exist.store(true, std::sync::atomic::Ordering::SeqCst);
                    break;
                }
            }
        });

        while !exist2.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(200));
        }

        drop(app.runtime().handle().block_on(app.handle().shutdown()));
        app.blocking_wait_finished()
            .expect("No service should panic");

        let recovery_data = load_recovery_data(rocksdb::RocksBackendSettings {
            db_path: temp_dir.path().join("test_db"),
            read_only: false,
            column_family: None,
        })
        .expect("Should load recovery data from storage.");
        let recovery_settings = TxMempoolSettings {
            pool: MempoolSettings::default(),
            network_adapter: (),
            seen_items_subscription_channel_size: 64.try_into().unwrap(),
            recovery_data,
        };
        let recovered_state =
            <MockRecoveryBackend as RecoveryBackendTrait<RuntimeServiceId>>::load_state(
                &recovery_settings,
            )
            .expect("Should not fail to load the state.")
            .expect("Recovery state should exist.");
        assert_eq!(recovered_state.pool().unwrap().pending_items.len(), 2);
        assert!(recovered_state.pool().unwrap().last_item_timestamp > 0);
    });
}

/// The prefix index is derived state, so the risk is that it drifts out of
/// sync with the pending set. These drive the real service message end to end:
/// index lookup, storage fetch, stream.
#[test]
fn prefix_lookup_tracks_pending_and_removed_txs() {
    run_with_mock_pool_node(Vec::new(), |settings, _temp_dir| {
        let app = OverwatchRunner::<MockPoolNode>::run(settings, None)
            .map_err(|e| eprintln!("Error encountered: {e}"))
            .unwrap();

        drop(
            app.runtime()
                .handle()
                .block_on(app.handle().start_all_services()),
        );

        let mempool_outbound = app
            .runtime()
            .handle()
            .block_on(async { app.handle().relay::<MockMempoolService>().await.unwrap() });

        let tx = tx("indexed");
        let prefix = tx.hash().key_prefix();

        // Unknown prefix resolves to nothing.
        assert!(
            app.runtime()
                .handle()
                .block_on(txs_by_prefix(&mempool_outbound, prefix))
                .is_empty(),
            "a prefix with no transaction must resolve to no candidates"
        );

        app.runtime()
            .handle()
            .block_on(add_tx(&mempool_outbound, tx.clone()))
            .expect("tx should be added");

        assert_eq!(
            app.runtime()
                .handle()
                .block_on(txs_by_prefix(&mempool_outbound, prefix)),
            vec![tx.clone()],
            "an added transaction must be reachable through its hash prefix"
        );

        // A prefix that no transaction carries stays empty.
        let absent = TxHashPrefix([0xFFu8; _]);
        if absent != prefix {
            assert!(
                app.runtime()
                    .handle()
                    .block_on(txs_by_prefix(&mempool_outbound, absent))
                    .is_empty(),
                "an unrelated prefix must not pick up candidates"
            );
        }

        app.runtime().handle().block_on(async {
            mempool_outbound
                .send(MempoolMsg::Remove {
                    ids: vec![tx.hash()],
                })
                .await
                .unwrap();
        });

        // A removed transaction stays reachable during its grace period, so a
        // competing proposal that references it can still be reconstructed.
        assert_eq!(
            app.runtime()
                .handle()
                .block_on(txs_by_prefix(&mempool_outbound, prefix)),
            vec![tx],
            "a removed transaction must stay reachable through its prefix during its grace period"
        );

        drop(app.runtime().handle().block_on(app.handle().shutdown()));
        app.blocking_wait_finished()
            .expect("No service should panic");
    });
}

/// The pool drives the sweep from processed blocks, so an eviction test is a
/// sequence of block notifications around the claim's deadline.
async fn processed_block(pool: &mut InMemoryPool, slot: u64) {
    pool.on_processed_block(
        HeaderId::from([0xAB; 32]),
        Slot::from(slot),
        Slot::genesis(),
    )
    .await;
}

#[tokio::test]
async fn expired_claim_is_evicted_after_its_window_and_grace() {
    let anchor = HeaderId::from([1u8; 32]);
    let storage = InMemoryStorageAdapter::default().with_block(anchor, 100);
    let mut pool = InMemoryPool::new(pow_claim_settings(), Era::genesis(), storage);

    let claim = claim_tx(anchor);
    let claim_id = claim.hash();
    let plain = tx("plain");
    let plain_id = plain.hash();
    pool.add_item(claim_id, claim.clone())
        .await
        .expect("claim tx should be added");
    pool.add_item(plain_id, plain)
        .await
        .expect("plain tx should be added");

    // Accepted through slot 110, rejected from 111, evicted from 131.
    processed_block(&mut pool, 130).await;
    assert_eq!(pool.status(&[claim_id]), vec![Status::Pending]);

    processed_block(&mut pool, 131).await;
    assert_eq!(pool.status(&[claim_id]), vec![Status::Unknown]);
    assert_eq!(pool.status(&[plain_id]), vec![Status::Pending]);
    // An evicted claim still resolves during its grace period.
    assert_eq!(
        pool.keys_by_prefix(&claim_id.key_prefix())
            .copied()
            .collect::<Vec<_>>(),
        vec![claim_id]
    );

    let fetched_after_eviction = pool
        .get_items_by_keys([claim_id])
        .await
        .expect("evicted tx should still be fetchable during grace period")
        .collect::<Vec<_>>()
        .await;
    assert_eq!(fetched_after_eviction, vec![claim]);
}

#[tokio::test]
async fn claim_that_arrives_expired_is_refused() {
    let anchor = HeaderId::from([1u8; 32]);
    let storage = InMemoryStorageAdapter::default().with_block(anchor, 100);
    let mut pool = InMemoryPool::new(pow_claim_settings(), Era::genesis(), storage);

    // The chain is already at the claim's deadline of slot 131.
    processed_block(&mut pool, 131).await;

    let claim = claim_tx(anchor);
    let claim_id = claim.hash();
    let error = pool
        .add_item(claim_id, claim)
        .await
        .expect_err("an expired claim must be refused");

    assert!(matches!(error, MempoolError::Expired));
    assert_eq!(pool.pending_item_count(), 0);
    assert_eq!(pool.status(&[claim_id]), vec![Status::Unknown]);
    assert!(pool.keys_by_prefix(&claim_id.key_prefix()).next().is_none());
    assert!(pool.save().pending_items.is_empty());

    let stored = pool
        .get_items_by_keys([claim_id])
        .await
        .expect("storage lookup should succeed")
        .collect::<Vec<_>>()
        .await;
    assert!(stored.is_empty(), "a refused claim must not reach storage");

    // One slot earlier it is still admitted, and then swept at the deadline.
    let storage = InMemoryStorageAdapter::default().with_block(anchor, 100);
    let mut pool = InMemoryPool::new(pow_claim_settings(), Era::genesis(), storage);
    processed_block(&mut pool, 130).await;
    pool.add_item(claim_id, claim_tx(anchor))
        .await
        .expect("a claim inside its grace must be admitted");
    assert_eq!(pool.status(&[claim_id]), vec![Status::Pending]);
    processed_block(&mut pool, 131).await;
    assert_eq!(pool.status(&[claim_id]), vec![Status::Unknown]);
}

#[tokio::test]
async fn claim_with_unknown_anchor_waits_for_its_block() {
    let anchor = HeaderId::from([1u8; 32]);
    let mut pool = InMemoryPool::new(
        pow_claim_settings(),
        Era::genesis(),
        InMemoryStorageAdapter::default(),
    );

    let claim = claim_tx(anchor);
    let claim_id = claim.hash();
    pool.add_item(claim_id, claim)
        .await
        .expect("claim tx should be added");

    // The chain is far past any plausible deadline, but the anchor is unknown.
    processed_block(&mut pool, 1_000).await;
    assert_eq!(pool.status(&[claim_id]), vec![Status::Pending]);

    // Once the anchor is processed its deadline resolves to slot 131, which
    // the chain already passed.
    pool.on_processed_block(anchor, Slot::from(100u64), Slot::genesis())
        .await;
    assert_eq!(pool.status(&[claim_id]), vec![Status::Unknown]);
}

/// A failed anchor lookup is logged and the claim parked, so the claims
/// whose anchors did resolve are still tracked and the failed one still
/// resolves once its block is reported processed.
#[tokio::test]
async fn failed_anchor_lookup_keeps_the_resolved_anchors() {
    let resolved = HeaderId::from([1u8; 32]);
    let failing = HeaderId::from([2u8; 32]);
    let storage = InMemoryStorageAdapter::default()
        .with_block(resolved, 100)
        .with_failing_block(failing);
    let mut pool = InMemoryPool::new(pow_claim_settings(), Era::genesis(), storage);

    let both = Ops::from([
        Op::ClaimPowReward(ClaimPowRewardOp {
            block_hash: resolved.into(),
            ..ClaimPowRewardOp::sample()
        }),
        Op::ClaimPowReward(ClaimPowRewardOp {
            block_hash: failing.into(),
            ..ClaimPowRewardOp::sample()
        }),
    ]);
    let both_id = both.hash();
    let failing_only = claim_tx(failing);
    let failing_only_id = failing_only.hash();
    pool.add_item(both_id, both)
        .await
        .expect("a claim with a failed anchor lookup should still be added");
    pool.add_item(failing_only_id, failing_only)
        .await
        .expect("a claim with a failed anchor lookup should still be added");

    // The resolved anchor keeps its deadline of slot 131; the other claim
    // has no deadline until its block is processed.
    processed_block(&mut pool, 130).await;
    assert_eq!(
        pool.status(&[both_id, failing_only_id]),
        vec![Status::Pending, Status::Pending]
    );
    processed_block(&mut pool, 131).await;
    assert_eq!(
        pool.status(&[both_id, failing_only_id]),
        vec![Status::Unknown, Status::Pending]
    );
    processed_block(&mut pool, 1_000).await;
    assert_eq!(pool.status(&[failing_only_id]), vec![Status::Pending]);

    pool.on_processed_block(failing, Slot::from(100u64), Slot::genesis())
        .await;
    assert_eq!(pool.status(&[failing_only_id]), vec![Status::Unknown]);
}

#[tokio::test]
async fn claim_eviction_is_disabled_without_settings() {
    let anchor = HeaderId::from([1u8; 32]);
    let storage = InMemoryStorageAdapter::default().with_block(anchor, 0);
    let mut pool = InMemoryPool::new(
        MempoolSettings {
            tx_ttl: None,
            pow_claim: None,
        },
        Era::genesis(),
        storage,
    );

    let claim = claim_tx(anchor);
    let claim_id = claim.hash();
    pool.add_item(claim_id, claim)
        .await
        .expect("claim tx should be added");

    processed_block(&mut pool, u64::MAX).await;
    assert_eq!(pool.status(&[claim_id]), vec![Status::Pending]);
}

#[tokio::test]
async fn claim_included_in_a_block_is_not_evicted_twice() {
    let anchor = HeaderId::from([1u8; 32]);
    let storage = InMemoryStorageAdapter::default().with_block(anchor, 100);
    let mut pool = InMemoryPool::new(pow_claim_settings(), Era::genesis(), storage);

    let claim = claim_tx(anchor);
    let claim_id = claim.hash();
    pool.add_item(claim_id, claim)
        .await
        .expect("claim tx should be added");

    pool.remove(&[claim_id]).await;
    let removed_at = pool.save().removed_items[&claim_id];

    processed_block(&mut pool, 1_000).await;
    assert_eq!(pool.status(&[claim_id]), vec![Status::Unknown]);
    assert_eq!(
        pool.save().removed_items[&claim_id],
        removed_at,
        "an already removed tx must not be retired again by the sweep"
    );
}
