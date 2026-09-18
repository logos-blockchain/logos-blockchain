//! `RocksDB` storage service and its typed API.

pub mod api;
pub mod backend;
mod metrics;
pub mod recovery;
pub mod rocksdb;

use std::{fmt::Display, time::Instant};

pub use api::requests::StorageMsg;
use async_trait::async_trait;
use backend::StorageBackend as _;
use lb_log_targets::storage;
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        state::{NoOperator, NoState},
    },
};

use crate::rocksdb::{RocksBackend, RocksBackendSettings, handle_request};

const LOG_TARGET: &str = storage::ROOT;

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
    pub async fn handle_storage_message(msg: StorageMsg, backend: &mut RocksBackend) {
        let started_at = Instant::now();

        let result = handle_request(msg, backend).await;

        if let Err(err) = result {
            metrics::storage_request_failed();
            tracing::error!(target: LOG_TARGET, err = %err, "Storage request failed");
        } else {
            metrics::storage_observe_request_ok(started_at);
        }
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
            mut backend,
            service_resources_handle:
                OpaqueServiceResourcesHandle::<Self, RuntimeServiceId> {
                    mut inbound_relay,
                    status_updater,
                    ..
                },
        } = self;
        let backend = &mut backend;

        status_updater.notify_ready();
        tracing::info!(
            target: LOG_TARGET,
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        while let Some(msg) = inbound_relay.recv().await {
            Self::handle_storage_message(msg, backend).await;
        }

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
