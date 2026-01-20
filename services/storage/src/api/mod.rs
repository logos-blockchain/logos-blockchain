use async_trait::async_trait;

use crate::{
    StorageServiceError,
    api::{
        chain::{StorageChainApi, requests::ChainApiRequest},
        da::{StorageDaApi, requests::DaApiRequest},
        membership::{StorageMembershipApi, requests::MembershipApiRequest},
    },
    backends::StorageBackend,
};

pub mod backend;
pub mod chain;
pub mod da;
pub mod membership;

#[async_trait]
pub trait StorageBackendApi: StorageChainApi + StorageDaApi + StorageMembershipApi {}

pub(crate) trait StorageOperation<Backend: StorageBackend> {
    async fn execute(self, api: &mut Backend) -> Result<(), StorageServiceError>;
}

pub enum StorageApiRequest<Backend: StorageBackend> {
    Chain(ChainApiRequest<Backend>),
    Da(DaApiRequest<Backend>),
    Membership(MembershipApiRequest),
}

impl<Backend: StorageBackend> StorageOperation<Backend> for StorageApiRequest<Backend> {
    async fn execute(self, backend: &mut Backend) -> Result<(), StorageServiceError> {
        match self {
            Self::Chain(request) => request.execute(backend).await,
            Self::Da(request) => request.execute(backend).await,
            Self::Membership(request) => request.execute(backend).await,
        }
    }
}
