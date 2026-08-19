use std::{
    collections::{BTreeMap, HashSet},
    convert::Infallible,
    pin::Pin,
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::Duration,
};

use async_trait::async_trait;
use futures::{Stream, StreamExt as _, stream};
use indexmap::IndexMap;
use lb_core::{
    block::MAX_BLOCK_TRANSACTIONS_SIZE,
    codec::{DeserializeOp as _, SerializeOp as _},
    header::HeaderId,
    mantle::{
        mock::{MockTransaction, MockTxId},
        transactions::hash::{PrefixedKey as _, TxHashPrefix},
    },
};
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
    backends::rocksdb::{self, RocksBackend},
    recovery::{StorageRecoveryBackend, load_recovery_data},
};
use lb_tracing_service::{Tracing, TracingSettings};
use lb_utils::noop_service::NoService;
use logos_blockchain_tx_service::{
    MempoolMsg, TxMempoolSettings,
    backend::{
        MemPool as _, Mempool, MempoolError, MempoolSettings, PoolRecoveryState,
        RecoverableMempool as _, Status,
    },
    network::adapters::mock::{MOCK_TX_CONTENT_TOPIC, MockAdapter},
    storage::{MempoolStorageAdapter, adapters::rocksdb::RocksStorageAdapter},
    tx::{service::GenericTxMempoolService, state::TxMempoolState},
};
use overwatch::{
    overwatch::OverwatchRunner,
    services::{ServiceData, relay::OutboundRelay},
};
use overwatch_derive::*;
use tempfile::TempDir;

type MockRecoveryBackend = StorageRecoveryBackend<
    TxMempoolState<PoolRecoveryState<MockTxId>, MempoolSettings, ()>,
    TxMempoolSettings<MempoolSettings, ()>,
    RocksBackend,
    RuntimeServiceId,
>;

type MockMempoolService = GenericTxMempoolService<
    Mempool<
        HeaderId,
        MockTransaction<MockMessage>,
        MockTxId,
        RocksStorageAdapter<MockTransaction<MockMessage>, MockTxId>,
        RuntimeServiceId,
    >,
    MockAdapter<RuntimeServiceId>,
    MockRecoveryBackend,
    RocksStorageAdapter<MockTransaction<MockMessage>, MockTxId>,
    RuntimeServiceId,
>;

#[derive_services]
struct MockPoolNode {
    logging: Tracing<RuntimeServiceId>,
    network: NetworkService<Mock, RuntimeServiceId>,
    storage: StorageService<RocksBackend, RuntimeServiceId>,
    mockpool: MockMempoolService,
    no_service: NoService,
}

fn sample_removed_tx() -> MockTransaction<MockMessage> {
    MockTransaction::new(MockMessage {
        payload: "removed-but-fetchable".to_owned(),
        content_topic: MOCK_TX_CONTENT_TOPIC,
        version: 0,
        timestamp: 0,
    })
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
                recovery_data: RecoveryData::default(),
            },
            logging: TracingSettings::default(),
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
    tx: MockTransaction<MockMessage>,
) -> Result<(), MempoolError> {
    let (reply_channel, reply) = tokio::sync::oneshot::channel();
    mempool_outbound
        .send(MempoolMsg::Add {
            key: tx.id(),
            payload: tx,
            reply_channel,
        })
        .await
        .unwrap();

    reply.await.expect("mempool should reply to local add")
}

async fn pending_txs(
    mempool_outbound: &OutboundRelay<<MockMempoolService as ServiceData>::Message>,
) -> Vec<MockTransaction<MockMessage>> {
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
) -> Vec<MockTransaction<MockMessage>> {
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
    items: Arc<Mutex<BTreeMap<MockTxId, MockTransaction<MockMessage>>>>,
}

#[derive(Clone, Default)]
struct FailingStorageAdapter;

#[async_trait]
impl MempoolStorageAdapter<RuntimeServiceId> for InMemoryStorageAdapter {
    type Backend = RocksBackend;
    type Item = MockTransaction<MockMessage>;
    type Key = MockTxId;
    type Error = Infallible;

    fn new(
        _storage_relay: OutboundRelay<
            <StorageService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
        >,
    ) -> Self {
        Self::default()
    }

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
}

#[async_trait]
impl MempoolStorageAdapter<RuntimeServiceId> for FailingStorageAdapter {
    type Backend = RocksBackend;
    type Item = MockTransaction<MockMessage>;
    type Key = MockTxId;
    type Error = MempoolError;

    fn new(
        _storage_relay: OutboundRelay<
            <StorageService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
        >,
    ) -> Self {
        Self
    }

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
}

#[test]
fn test_mock_pool_recovery_state() {
    let recovery_state = PoolRecoveryState::<MockTxId> {
        pending_items: IndexMap::new(),
        removed_items: BTreeMap::new(),
        last_item_timestamp: 1_234_567_890,
    };

    let serialized = recovery_state.to_bytes().expect("Should serialize");

    let deserialized: PoolRecoveryState<MockTxId> =
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
    }
}

const TEST_TX_TTL: Duration = Duration::from_hours(1);

/// Build a pool holding one pending tx whose insertion timestamp is backdated
/// to the unix epoch, so it is expired for any reasonable TTL.
async fn pool_with_backdated_tx(
    settings: MempoolSettings,
) -> (
    Mempool<
        HeaderId,
        MockTransaction<MockMessage>,
        MockTxId,
        InMemoryStorageAdapter,
        RuntimeServiceId,
    >,
    MockTransaction<MockMessage>,
) {
    let storage = InMemoryStorageAdapter::default();
    let mut pool = Mempool::<
        HeaderId,
        MockTransaction<MockMessage>,
        MockTxId,
        InMemoryStorageAdapter,
        RuntimeServiceId,
    >::new(settings, storage.clone());

    let tx = sample_removed_tx();
    let tx_id = tx.id();

    pool.add_item(tx_id, tx.clone())
        .await
        .expect("tx should be added");

    let mut saved_state = pool.save();
    saved_state.pending_items.insert(tx_id, 0);

    (Mempool::recover(settings, saved_state, storage), tx)
}

#[tokio::test]
async fn expired_tx_is_evicted_on_next_sweep() {
    let storage = InMemoryStorageAdapter::default();
    let mut pool = Mempool::<
        HeaderId,
        MockTransaction<MockMessage>,
        MockTxId,
        InMemoryStorageAdapter,
        RuntimeServiceId,
    >::new(ttl_settings(TEST_TX_TTL), storage.clone());

    let old_tx = sample_removed_tx();
    let old_tx_id = old_tx.id();
    let old_tx_prefix = old_tx_id.key_prefix();

    let fresh_tx = MockTransaction::new(MockMessage {
        payload: "fresh".to_owned(),
        content_topic: MOCK_TX_CONTENT_TOPIC,
        version: 0,
        timestamp: 1,
    });
    let fresh_tx_id = fresh_tx.id();

    pool.add_item(old_tx_id, old_tx.clone())
        .await
        .expect("old tx should be added");
    pool.add_item(fresh_tx_id, fresh_tx.clone())
        .await
        .expect("fresh tx should be added");

    let mut saved_state = pool.save();
    saved_state.pending_items.insert(old_tx_id, 0);
    let mut pool = Mempool::<
        HeaderId,
        MockTransaction<MockMessage>,
        MockTxId,
        InMemoryStorageAdapter,
        RuntimeServiceId,
    >::recover(ttl_settings(TEST_TX_TTL), saved_state, storage);

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
    assert!(pool.keys_by_prefix(&old_tx_prefix).next().is_none());

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
    let (mut pool, tx) = pool_with_backdated_tx(MempoolSettings { tx_ttl: None }).await;
    let tx_id = tx.id();

    for _ in 0..10 {
        pool.remove(&[]).await;
    }

    assert_eq!(pool.pending_item_count(), 1);
    assert_eq!(pool.status(&[tx_id]), vec![Status::Pending]);
}

#[tokio::test]
async fn storage_failure_does_not_mark_tx_pending() {
    let mut pool = Mempool::<
        HeaderId,
        MockTransaction<MockMessage>,
        MockTxId,
        FailingStorageAdapter,
        RuntimeServiceId,
    >::new(MempoolSettings::default(), FailingStorageAdapter);

    let tx = sample_removed_tx();
    let tx_id = tx.id();

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
    let mut pool = Mempool::<
        HeaderId,
        MockTransaction<MockMessage>,
        MockTxId,
        InMemoryStorageAdapter,
        RuntimeServiceId,
    >::new(MempoolSettings::default(), storage.clone());

    let tx = sample_removed_tx();
    let tx_id = tx.id();

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
    assert!(pending_after_remove.is_empty());

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
    let mut pool = Mempool::<
        HeaderId,
        MockTransaction<MockMessage>,
        MockTxId,
        InMemoryStorageAdapter,
        RuntimeServiceId,
    >::new(MempoolSettings::default(), storage.clone());

    let tx = sample_removed_tx();
    let tx_id = tx.id();

    pool.add_item(tx_id, tx.clone())
        .await
        .expect("tx should be added");
    pool.remove(&[tx_id]).await;

    let saved_state = pool.save();
    assert!(saved_state.pending_items.is_empty());
    assert!(saved_state.removed_items.contains_key(&tx_id));

    let recovered_pool = Mempool::<
        HeaderId,
        MockTransaction<MockMessage>,
        MockTxId,
        InMemoryStorageAdapter,
        RuntimeServiceId,
    >::recover(MempoolSettings::default(), saved_state, storage);

    assert_eq!(recovered_pool.pending_item_count(), 0);
    assert_eq!(recovered_pool.status(&[tx_id]), vec![Status::Unknown]);

    let fetched_after_recovery = recovered_pool
        .get_items_by_keys([tx_id])
        .await
        .expect("removed tx should still be fetchable after recovery")
        .collect::<Vec<_>>()
        .await;
    assert_eq!(fetched_after_recovery, vec![tx]);
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

        let oversized_tx = MockTransaction::new(MockMessage {
            payload: "x".repeat(MAX_BLOCK_TRANSACTIONS_SIZE + 1),
            content_topic: MOCK_TX_CONTENT_TOPIC,
            version: 0,
            timestamp: 0,
        });

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

        assert!(
            app.runtime()
                .handle()
                .block_on(pending_txs(&mempool_outbound))
                .is_empty()
        );

        drop(app.runtime().handle().block_on(app.handle().shutdown()));
        app.blocking_wait_finished();
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

        let first_tx = MockTransaction::new(MockMessage {
            payload: "first".to_owned(),
            content_topic: MOCK_TX_CONTENT_TOPIC,
            version: 0,
            timestamp: 1,
        });

        let second_tx = MockTransaction::new(MockMessage {
            payload: "second".to_owned(),
            content_topic: MOCK_TX_CONTENT_TOPIC,
            version: 0,
            timestamp: 2,
        });

        let expected_txs = if first_tx.id() < second_tx.id() {
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
        app.blocking_wait_finished();
    });
}

#[test]
fn test_mock_mempool() {
    let exist = Arc::new(AtomicBool::new(false));
    let exist2 = Arc::clone(&exist);

    let predefined_messages = vec![
        MockMessage {
            payload: "This is foo".to_owned(),
            content_topic: MOCK_TX_CONTENT_TOPIC,
            version: 0,
            timestamp: 0,
        },
        MockMessage {
            payload: "This is bar".to_owned(),
            content_topic: MOCK_TX_CONTENT_TOPIC,
            version: 0,
            timestamp: 0,
        },
    ];

    run_with_mock_pool_node(predefined_messages.clone(), |settings, temp_dir| {
        let exp_txns: HashSet<MockMessage> = predefined_messages.iter().cloned().collect();
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
                .relay::<NetworkService<_, _>>()
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

                let items: HashSet<MockMessage> = mrx
                    .await
                    .unwrap()
                    .map(|msg| msg.message().clone())
                    .collect()
                    .await;

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
        app.blocking_wait_finished();

        let recovery_data = load_recovery_data(rocksdb::RocksBackendSettings {
            db_path: temp_dir.path().join("test_db"),
            read_only: false,
            column_family: None,
        })
        .expect("Should load recovery data from storage.");
        let recovery_settings = TxMempoolSettings {
            pool: MempoolSettings::default(),
            network_adapter: (),
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
fn prefix_lookup_tracks_the_pending_set() {
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

        let tx = MockTransaction::new(MockMessage {
            payload: "indexed".to_owned(),
            content_topic: MOCK_TX_CONTENT_TOPIC,
            version: 0,
            timestamp: 1,
        });
        let prefix = tx.id().key_prefix();

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
                .send(MempoolMsg::Remove { ids: vec![tx.id()] })
                .await
                .unwrap();
        });

        // Removal must evict from the index, not just the pending set.
        assert!(
            app.runtime()
                .handle()
                .block_on(txs_by_prefix(&mempool_outbound, prefix))
                .is_empty(),
            "a removed transaction must no longer be reachable through its prefix"
        );

        drop(app.runtime().handle().block_on(app.handle().shutdown()));
        app.blocking_wait_finished();
    });
}
