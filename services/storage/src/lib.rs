pub mod api;
pub mod backends;

use std::{
    fmt::{Debug, Display, Formatter},
    marker::PhantomData,
    num::NonZeroUsize,
};

use async_trait::async_trait;
use backends::{StorageBackend, StorageTransaction};
use bytes::Bytes;
use lb_core::codec::{DeserializeOp as _, SerializeOp as _};
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        state::{NoOperator, NoState},
    },
};
use serde::{Serialize, de::DeserializeOwned};

use crate::api::{StorageApiRequest, StorageOperation};

/// Storage message that maps to [`StorageBackend`] trait
pub enum StorageMsg<Backend: StorageBackend> {
    Load {
        key: Bytes,
        reply_channel: tokio::sync::oneshot::Sender<Option<Bytes>>,
    },
    LoadPrefix {
        prefix: Bytes,
        start_key: Option<Bytes>,
        end_key: Option<Bytes>,
        limit: Option<NonZeroUsize>,
        reply_channel: tokio::sync::oneshot::Sender<Vec<Bytes>>,
    },
    Store {
        key: Bytes,
        value: Bytes,
    },
    Remove {
        key: Bytes,
        reply_channel: tokio::sync::oneshot::Sender<Option<Bytes>>,
    },
    Execute {
        transaction: Backend::Transaction,
        reply_channel:
            tokio::sync::oneshot::Sender<<Backend::Transaction as StorageTransaction>::Result>,
    },
    Api {
        request: StorageApiRequest<Backend>,
    },
}

/// Reply channel for storage messages
pub struct StorageReplyReceiver<T, Backend> {
    channel: tokio::sync::oneshot::Receiver<T>,
    _backend: PhantomData<Backend>,
}

impl<T, Backend> StorageReplyReceiver<T, Backend> {
    #[must_use]
    pub const fn new(channel: tokio::sync::oneshot::Receiver<T>) -> Self {
        Self {
            channel,
            _backend: PhantomData,
        }
    }

    #[must_use]
    pub fn into_inner(self) -> tokio::sync::oneshot::Receiver<T> {
        self.channel
    }
}

impl<Backend: StorageBackend> StorageReplyReceiver<Option<Bytes>, Backend> {
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

impl<Backend: StorageBackend> StorageMsg<Backend> {
    pub fn new_load_message(key: Bytes) -> (Self, StorageReplyReceiver<Option<Bytes>, Backend>) {
        let (reply_channel, receiver) = tokio::sync::oneshot::channel();
        (
            Self::Load { key, reply_channel },
            StorageReplyReceiver::new(receiver),
        )
    }

    pub fn new_store_message<V: Serialize + DeserializeOwned>(
        key: Bytes,
        value: &V,
    ) -> Result<Self, lb_core::codec::Error> {
        let value = value.to_bytes()?;
        Ok(Self::Store { key, value })
    }

    pub fn new_remove_message(key: Bytes) -> (Self, StorageReplyReceiver<Option<Bytes>, Backend>) {
        let (reply_channel, receiver) = tokio::sync::oneshot::channel();
        (
            Self::Remove { key, reply_channel },
            StorageReplyReceiver::new(receiver),
        )
    }

    pub fn new_transaction_message(
        transaction: Backend::Transaction,
    ) -> (
        Self,
        StorageReplyReceiver<<Backend::Transaction as StorageTransaction>::Result, Backend>,
    ) {
        let (reply_channel, receiver) = tokio::sync::oneshot::channel();
        (
            Self::Execute {
                transaction,
                reply_channel,
            },
            StorageReplyReceiver::new(receiver),
        )
    }
}

// Implement `Debug` manually to avoid constraining `Backend` to `Debug`
impl<Backend: StorageBackend> Debug for StorageMsg<Backend> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Load { key, .. } => {
                write!(f, "Load {{ {key:?} }}")
            }
            Self::LoadPrefix {
                prefix,
                start_key,
                end_key,
                limit,
                ..
            } => {
                write!(
                    f,
                    "LoadPrefix {{ {prefix:?} {start_key:?}, {end_key:?}, limit: {limit:?} }}"
                )
            }
            Self::Store { key, value } => {
                write!(f, "Store {{ {key:?}, {value:?}}}")
            }
            Self::Remove { key, .. } => {
                write!(f, "Remove {{ {key:?} }}")
            }
            Self::Execute { .. } => write!(f, "Execute transaction"),
            Self::Api { .. } => {
                write!(f, "Api {{ .. }}")
            }
        }
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

/// Storage service that wraps a [`StorageBackend`]
pub struct StorageService<Backend, RuntimeServiceId>
where
    Backend: StorageBackend + Send + Sync + 'static,
{
    backend: Backend,
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
}

impl<Backend, RuntimeServiceId> StorageService<Backend, RuntimeServiceId>
where
    Backend: StorageBackend + Send + Sync + 'static,
{
    async fn handle_storage_message(msg: StorageMsg<Backend>, backend: &mut Backend) {
        if let Err(e) = match msg {
            StorageMsg::Load { key, reply_channel } => {
                Self::handle_load(backend, key, reply_channel).await
            }
            StorageMsg::LoadPrefix {
                prefix,
                start_key,
                end_key,
                limit,
                reply_channel,
            } => {
                Self::handle_load_prefix(backend, prefix, start_key, end_key, limit, reply_channel)
                    .await
            }
            StorageMsg::Store { key, value } => Self::handle_store(backend, key, value).await,
            StorageMsg::Remove { key, reply_channel } => {
                Self::handle_remove(backend, key, reply_channel).await
            }
            StorageMsg::Execute {
                transaction,
                reply_channel,
            } => Self::handle_execute(backend, transaction, reply_channel).await,
            StorageMsg::Api { request: api_call } => Self::handle_api_call(api_call, backend).await,
        } {
            tracing::error!("Error handling storage message: {e}");
        }
    }
    /// Handle load message
    async fn handle_load(
        backend: &mut Backend,
        key: Bytes,
        reply_channel: tokio::sync::oneshot::Sender<Option<Bytes>>,
    ) -> Result<(), StorageServiceError> {
        let result: Option<Bytes> = backend
            .load(&key)
            .await
            .map_err(|e| StorageServiceError::BackendError(e.into()))?;
        reply_channel
            .send(result)
            .map_err(|_| StorageServiceError::ReplyError {
                message: format!("Load {key:?}"),
            })
    }

    /// Handle load prefix message
    async fn handle_load_prefix(
        backend: &mut Backend,
        prefix: Bytes,
        start_key: Option<Bytes>,
        end_key: Option<Bytes>,
        limit: Option<NonZeroUsize>,
        reply_channel: tokio::sync::oneshot::Sender<Vec<Bytes>>,
    ) -> Result<(), StorageServiceError> {
        let result: Vec<Bytes> = backend
            .load_prefix(&prefix, start_key.as_deref(), end_key.as_deref(), limit)
            .await
            .map_err(|e| StorageServiceError::BackendError(e.into()))?;
        reply_channel
            .send(result)
            .map_err(|_| StorageServiceError::ReplyError {
                message: format!("LoadPrefix {prefix:?}"),
            })
    }

    /// Handle remove message
    async fn handle_remove(
        backend: &mut Backend,
        key: Bytes,
        reply_channel: tokio::sync::oneshot::Sender<Option<Bytes>>,
    ) -> Result<(), StorageServiceError> {
        let result: Option<Bytes> = backend
            .remove(&key)
            .await
            .map_err(|e| StorageServiceError::BackendError(e.into()))?;
        reply_channel
            .send(result)
            .map_err(|_| StorageServiceError::ReplyError {
                message: format!("Remove {key:?}"),
            })
    }

    /// Handle store message
    async fn handle_store(
        backend: &mut Backend,
        key: Bytes,
        value: Bytes,
    ) -> Result<(), StorageServiceError> {
        backend
            .store(key, value)
            .await
            .map_err(|e| StorageServiceError::BackendError(e.into()))
    }

    /// Handle execute message
    async fn handle_execute(
        backend: &mut Backend,
        transaction: Backend::Transaction,
        reply_channel: tokio::sync::oneshot::Sender<
            <Backend::Transaction as StorageTransaction>::Result,
        >,
    ) -> Result<(), StorageServiceError> {
        let result = backend
            .execute(transaction)
            .await
            .map_err(|e| StorageServiceError::BackendError(e.into()))?;
        reply_channel
            .send(result)
            .map_err(|_| StorageServiceError::ReplyError {
                message: "Execute transaction".to_owned(),
            })
    }

    async fn handle_api_call(
        api_call: StorageApiRequest<Backend>,
        api_backend: &mut Backend,
    ) -> Result<(), StorageServiceError> {
        <StorageApiRequest<Backend> as StorageOperation<Backend>>::execute(api_call, api_backend)
            .await
            .map_err(|e| StorageServiceError::BackendError(e.into()))
    }
}

#[async_trait]
impl<Backend, RuntimeServiceId> ServiceCore<RuntimeServiceId>
    for StorageService<Backend, RuntimeServiceId>
where
    Backend: StorageBackend + Send + Sync + 'static,
    RuntimeServiceId: AsServiceId<Self> + Display + Send,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        _initial_state: Self::State,
    ) -> Result<Self, DynError> {
        Ok(Self {
            backend: Backend::new(
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

impl<Backend, RuntimeServiceId> ServiceData for StorageService<Backend, RuntimeServiceId>
where
    Backend: StorageBackend + Send + Sync + 'static,
{
    type Settings = Backend::Settings;
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = StorageMsg<Backend>;
}
