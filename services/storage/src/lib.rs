//! `RocksDB` storage service and its typed API.
//!
//! Enable `rocksdb-backend` to use this crate.
#![cfg(feature = "rocksdb-backend")]

pub mod api;
pub mod backend;
mod metrics;
pub mod recovery;
pub mod rocksdb;

use std::{fmt::Display, time::Instant};

pub use api::requests::StorageMsg;
use async_trait::async_trait;
use backend::StorageBackend as _;
use bytes::Bytes;
use lb_binary_codec::bincode::DeserializeOp as _;
use lb_log_targets::storage;
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        state::{NoOperator, NoState},
    },
};
use serde::{Serialize, de::DeserializeOwned};

use crate::rocksdb::{RocksBackend, RocksBackendSettings, handle_request};

const LOG_TARGET: &str = storage::ROOT;

/// Reply channel for storage messages
pub struct StorageReplyReceiver<T> {
    channel: tokio::sync::oneshot::Receiver<T>,
}

impl<T> StorageReplyReceiver<T> {
    #[must_use]
    pub const fn new(channel: tokio::sync::oneshot::Receiver<T>) -> Self {
        Self { channel }
    }

    #[must_use]
    pub fn into_inner(self) -> tokio::sync::oneshot::Receiver<T> {
        self.channel
    }
}

impl StorageReplyReceiver<Option<Bytes>> {
    /// Receive and transform the reply into the desired type
    /// Target type must implement `From` from the original backend stored type.
    pub async fn recv<Output>(
        self,
    ) -> Result<Option<Output>, tokio::sync::oneshot::error::RecvError>
    where
        Output: Serialize + DeserializeOwned,
    {
        self.channel
            .await
            // TODO: This should probably just return a result anyway. But for now we can consider
            // in infallible.
            .map(|maybe_bytes| {
                maybe_bytes.map(|bytes| {
                    Output::from_bytes(&bytes).expect("Recovery from storage should never fail")
                })
            })
    }
}

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
