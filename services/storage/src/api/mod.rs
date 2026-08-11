use async_trait::async_trait;

use crate::{
    StorageServiceError,
    api::chain::{StorageChainApi, requests::ChainApiRequest},
    backends::StorageBackend,
};

pub mod backend;
pub mod chain;

#[async_trait]
pub trait StorageBackendApi: StorageChainApi {}

pub(crate) trait StorageOperation<Backend: StorageBackend> {
    async fn execute(self, api: &mut Backend) -> Result<(), StorageServiceError>;
}

pub enum StorageApiRequest<Backend: StorageBackend> {
    Chain(ChainApiRequest<Backend>),
}

impl<Backend: StorageBackend> StorageOperation<Backend> for StorageApiRequest<Backend> {
    async fn execute(self, backend: &mut Backend) -> Result<(), StorageServiceError> {
        match self {
            Self::Chain(request) => request.execute(backend).await,
        }
    }
}
