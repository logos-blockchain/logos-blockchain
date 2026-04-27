use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    convert::Infallible,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex, atomic::AtomicBool},
};

use async_trait::async_trait;
use futures::{Stream, StreamExt as _, stream};
use lb_core::{
    codec::{DeserializeOp as _, SerializeOp as _},
    header::HeaderId,
    mantle::mock::{MockTransaction, MockTxId},
};
use lb_network_service::{
    NetworkService,
    backends::mock::{Mock, MockBackendMessage, MockConfig, MockMessage},
    config::NetworkConfig,
    message::NetworkMsg,
};
use lb_services_utils::{
    overwatch::{JsonFileBackend, recovery::operators::RecoveryBackend as _},
    traits::FromSettings as _,
};
use lb_storage_service::{
    StorageService,
    backends::rocksdb::{self, RocksBackend},
};
use lb_tracing_service::{Tracing, TracingSettings};
use lb_utils::noop_service::NoService;
use logos_blockchain_tx_service::{
    MempoolMsg, TxMempoolSettings,
    backend::{MemPool as _, Mempool, PoolRecoveryState, RecoverableMempool as _, Status},
    network::adapters::mock::{MOCK_TX_CONTENT_TOPIC, MockAdapter},
    storage::{MempoolStorageAdapter, adapters::rocksdb::RocksStorageAdapter},
    tx::{service::GenericTxMempoolService, state::TxMempoolState},
};
use overwatch::{
    overwatch::OverwatchRunner,
    services::{ServiceData, relay::OutboundRelay},
};
use overwatch_derive::*;
use rand::distributions::{Alphanumeric, DistString as _};
use tempfile::TempDir;

type MockRecoveryBackend =
    JsonFileBackend<TxMempoolState<PoolRecoveryState<MockTxId>, (), ()>, TxMempoolSettings<(), ()>>;

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

fn run_with_recovery_teardown(recovery_path: &Path, run: impl Fn()) {
    run();
    drop(std::fs::remove_file(recovery_path));
}

fn get_test_random_path() -> PathBuf {
    PathBuf::from(Alphanumeric.sample_string(&mut rand::thread_rng(), 5)).with_extension(".json")
}

fn sample_removed_tx() -> MockTransaction<MockMessage> {
    MockTransaction::new(MockMessage {
        payload: "removed-but-fetchable".to_owned(),
        content_topic: MOCK_TX_CONTENT_TOPIC,
        version: 0,
        timestamp: 0,
    })
}

#[derive(Clone, Default)]
struct InMemoryStorageAdapter {
    items: Arc<Mutex<BTreeMap<MockTxId, MockTransaction<MockMessage>>>>,
}

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
        keys: &BTreeSet<Self::Key>,
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

#[test]
fn test_mock_pool_recovery_state() {
    let recovery_state = PoolRecoveryState::<MockTxId> {
        pending_items: BTreeSet::new(),
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

#[tokio::test]
async fn removed_items_are_not_pending_but_still_fetchable() {
    let storage = InMemoryStorageAdapter::default();
    let mut pool = Mempool::<
        HeaderId,
        MockTransaction<MockMessage>,
        MockTxId,
        InMemoryStorageAdapter,
        RuntimeServiceId,
    >::new((), storage.clone());

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
    >::new((), storage.clone());

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
    >::recover((), saved_state, storage);

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
#[expect(clippy::too_many_lines, reason = "self contained test")]
fn test_mock_mempool() {
    let recovery_file_path = get_test_random_path();
    run_with_recovery_teardown(&recovery_file_path, || {
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

        let exp_txns: HashSet<MockMessage> = predefined_messages.iter().cloned().collect();

        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let db_path = temp_dir.path().join("test_db");

        let settings = MockPoolNodeServiceSettings {
            network: NetworkConfig {
                backend: MockConfig {
                    predefined_messages,
                    duration: tokio::time::Duration::from_millis(100),
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
                pool: (),
                network_adapter: (),
                recovery_path: recovery_file_path.clone(),
            },
            logging: TracingSettings::default(),
            no_service: (),
        };
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
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
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
            std::thread::sleep(std::time::Duration::from_millis(200));
        }

        let recovery_backend = MockRecoveryBackend::from_settings(&TxMempoolSettings {
            pool: (),
            network_adapter: (),
            recovery_path: recovery_file_path.clone(),
        });
        let recovered_state = recovery_backend
            .load_state()
            .expect("Should not fail to load the state.");
        assert_eq!(recovered_state.pool().unwrap().pending_items.len(), 2);
        assert!(recovered_state.pool().unwrap().last_item_timestamp > 0);

        drop(app.runtime().handle().block_on(app.handle().shutdown()));
        app.blocking_wait_finished();
    });
}
