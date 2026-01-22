use async_trait::async_trait;

use crate::{
    StorageServiceError,
    api::{
        chain::{StorageChainApi, requests::ChainApiRequest},
        membership::{StorageMembershipApi, requests::MembershipApiRequest},
    },
    backends::StorageBackend,
};

pub mod backend;
pub mod chain;
pub mod membership;

#[async_trait]
pub trait StorageBackendApi: StorageChainApi + StorageMembershipApi {}

pub(crate) trait StorageOperation<Backend: StorageBackend> {
    async fn execute(self, api: &mut Backend) -> Result<(), StorageServiceError>;
}

pub enum StorageApiRequest<Backend: StorageBackend> {
    Chain(ChainApiRequest<Backend>),
    Membership(MembershipApiRequest),
}

impl<Backend: StorageBackend> StorageOperation<Backend> for StorageApiRequest<Backend> {
    async fn execute(self, backend: &mut Backend) -> Result<(), StorageServiceError> {
        match self {
            Self::Chain(request) => request.execute(backend).await,
            Self::Membership(request) => request.execute(backend).await,
        }
    }
}
