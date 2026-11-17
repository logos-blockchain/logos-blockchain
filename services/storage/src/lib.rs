//! `RocksDB` storage service and its typed API.

pub mod api;
pub mod backend;
mod metrics;
pub mod recovery;
pub mod rocksdb;

use std::{fmt::Display, sync::Arc, time::Instant};

pub use api::requests::StorageMsg;
use async_trait::async_trait;
use backend::StorageBackend as _;
use lb_log_targets::storage;
use lb_utils::tokio::task::spawn_blocking;
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        relay::InboundRelay,
        resources::ServiceResourcesHandle,
        state::{NoOperator, NoState},
    },
};
use tokio::sync::RwLock;

use crate::rocksdb::{RocksBackend, RocksBackendSettings, handle_request};

const LOG_TARGET: &str = storage::ROOT;

/// How many reads run at once. Each holds a blocking thread, and past this
/// requests wait in the relay, which pushes back on their senders.
const MAX_CONCURRENT_READS: u32 = 16;

/// Storage error
/// Errors that may happen when performing storage operations
#[derive(Debug, thiserror::Error)]
pub enum StorageServiceError {
    #[error("Couldn't send a reply [{message:?}]")]
    ReplyError { message: String },
    #[error("Storage backend error")]
    BackendError(Box<dyn std::error::Error + Send + Sync>),
}

/// Storage service backed by `RocksDB`.
pub struct StorageService<RuntimeServiceId> {
    backend: RocksBackend,
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
}

impl<RuntimeServiceId> StorageService<RuntimeServiceId> {
    pub fn handle_storage_message(msg: StorageMsg, backend: &RocksBackend) {
        let started_at = Instant::now();

        let result = handle_request(msg, backend);

        if let Err(err) = result {
            metrics::storage_request_failed();
            tracing::error!(target: LOG_TARGET, err = %err, "Storage request failed");
        } else {
            metrics::storage_observe_request_ok(started_at);
        }
    }

    /// Runs requests on the blocking pool, so that disk I/O holds neither an
    /// async worker nor the loop.
    ///
    /// Each request takes its turn from `turns` on arrival and holds it until
    /// it finishes: reads overlap, and a write runs alone. So writes land in
    /// arrival order, and a request sees every write that arrived before it,
    /// which callers count on for the writes they get no reply to.
    async fn serve(
        mut requests: InboundRelay<StorageMsg>,
        backend: RocksBackend,
        turns: Arc<RwLock<()>>,
    ) {
        while let Some(msg) = requests.recv().await {
            if msg.is_read() {
                Self::run_request(msg, backend.clone(), Arc::clone(&turns).read_owned().await);
            } else {
                Self::run_request(msg, backend.clone(), Arc::clone(&turns).write_owned().await);
            }
        }
    }

    fn run_request<Turn>(msg: StorageMsg, backend: RocksBackend, turn: Turn)
    where
        Turn: Send + 'static,
    {
        spawn_blocking("logos/storage/request-blocking", move || {
            Self::handle_storage_message(msg, &backend);
            drop(turn);
        });
    }
}

#[async_trait]
impl<RuntimeServiceId> ServiceCore<RuntimeServiceId> for StorageService<RuntimeServiceId>
where
    RuntimeServiceId: AsServiceId<Self> + Display + Send,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        _initial_state: Self::State,
    ) -> Result<Self, DynError> {
        Ok(Self {
            backend: RocksBackend::new(
                service_resources_handle
                    .settings_handle
                    .notifier()
                    .get_updated_settings(),
            )?,
            service_resources_handle,
        })
    }

    async fn run(mut self) -> Result<(), DynError> {
        let Self {
            backend,
            service_resources_handle:
                ServiceResourcesHandle {
                    inbound_relay,
                    status_updater,
                    ..
                },
        } = self;

        status_updater.notify_ready();
        tracing::info!(
            target: LOG_TARGET,
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        let turns = Arc::new(RwLock::with_max_readers((), MAX_CONCURRENT_READS));
        Self::serve(inbound_relay, backend, turns).await;

        Ok(())
        // TODO: Implement `Drop` to finish pending transactions and close
        //  connections gracefully.
    }
}

impl<RuntimeServiceId> ServiceData for StorageService<RuntimeServiceId> {
    type Settings = RocksBackendSettings;
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = StorageMsg;
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bytes::Bytes;
    use overwatch::services::relay::{OutboundRelay, Relay};
    use tempfile::TempDir;
    use tokio::{
        sync::oneshot,
        time::{sleep, timeout},
    };

    use super::*;

    /// Long enough for a request that isn't held back to have started.
    const SETTLE_TIME: Duration = Duration::from_millis(100);
    const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

    struct RunningService {
        requests: OutboundRelay<StorageMsg>,
        backend: RocksBackend,
        turns: Arc<RwLock<()>>,
        _directory: TempDir,
    }

    impl RunningService {
        fn start() -> Self {
            let directory = TempDir::new().unwrap();
            let backend = RocksBackend::new(RocksBackendSettings {
                db_path: directory.path().into(),
                read_only: false,
                column_family: None,
            })
            .unwrap();
            let Relay {
                inbound_relay,
                outbound_relay,
                ..
            } = Relay::new(16);
            let turns = Arc::new(RwLock::new(()));
            tokio::spawn(StorageService::<()>::serve(
                inbound_relay,
                backend.clone(),
                Arc::clone(&turns),
            ));

            Self {
                requests: outbound_relay,
                backend,
                turns,
                _directory: directory,
            }
        }

        async fn store(&self, value: &'static str) {
            self.requests
                .send(StorageMsg::Store {
                    key: "key".into(),
                    value: value.into(),
                })
                .await
                .unwrap();
        }

        async fn send_load(&self) -> oneshot::Receiver<Option<Bytes>> {
            let (reply_channel, reply) = oneshot::channel();
            self.requests
                .send(StorageMsg::Load {
                    key: "key".into(),
                    reply_channel,
                })
                .await
                .unwrap();
            reply
        }
    }

    #[tokio::test]
    async fn reads_overlap() {
        let service = RunningService::start();
        let _slow_read = service.turns.read().await;

        let loaded = service.send_load().await;

        assert_eq!(timeout(REPLY_TIMEOUT, loaded).await.unwrap().unwrap(), None);
    }

    #[tokio::test]
    async fn a_read_sees_the_writes_that_arrived_before_it() {
        let service = RunningService::start();
        // Holds the write back, so that the read arrives while it's pending.
        let slow_read = service.turns.read().await;

        service.store("value").await;
        let mut loaded = service.send_load().await;
        sleep(SETTLE_TIME).await;
        assert!(loaded.try_recv().is_err());

        drop(slow_read);
        assert_eq!(
            timeout(REPLY_TIMEOUT, loaded).await.unwrap().unwrap(),
            Some("value".into())
        );
    }

    #[tokio::test]
    async fn writes_land_in_arrival_order() {
        let service = RunningService::start();
        let (release, released) = oneshot::channel::<()>();
        let first_write = service.backend.txn(move |database| {
            released.blocking_recv().unwrap();
            database.put("key", "first")?;
            Ok(None)
        });
        let (reply_channel, _reply) = oneshot::channel();
        service
            .requests
            .send(StorageMsg::Execute {
                transaction: first_write,
                reply_channel,
            })
            .await
            .unwrap();

        service.store("second").await;
        let loaded = service.send_load().await;
        sleep(SETTLE_TIME).await;
        release.send(()).unwrap();

        assert_eq!(
            timeout(REPLY_TIMEOUT, loaded).await.unwrap().unwrap(),
            Some("second".into())
        );
    }

    // `blocking_recv` panics on an async task, and on a single thread the
    // release could not run while the request held the runtime.
    #[tokio::test(flavor = "current_thread")]
    async fn a_slow_request_leaves_the_runtime_free() {
        let service = RunningService::start();
        let (release, released) = oneshot::channel::<()>();
        let transaction = service.backend.txn(move |_| {
            released.blocking_recv().unwrap();
            Ok(Some("done".into()))
        });
        let (reply_channel, reply) = oneshot::channel();
        service
            .requests
            .send(StorageMsg::Execute {
                transaction,
                reply_channel,
            })
            .await
            .unwrap();

        sleep(SETTLE_TIME).await;
        release.send(()).unwrap();

        assert_eq!(
            timeout(REPLY_TIMEOUT, reply)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            Some("done".into())
        );
    }
}
