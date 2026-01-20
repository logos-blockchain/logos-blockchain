pub mod adapters;

use lb_core::da::blob::Share;
use lb_storage_service::{StorageService, backends::StorageBackend};
use overwatch::{
    DynError,
    services::{ServiceData, relay::OutboundRelay},
};

#[async_trait::async_trait]
pub trait DaStorageAdapter<RuntimeServiceId> {
    type Backend: StorageBackend + Send + Sync + 'static;
    type Settings: Clone;
    type Share: Share + Clone;
    type Tx;

    async fn new(
        storage_relay: OutboundRelay<
            <StorageService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
        >,
    ) -> Self;

    async fn add_share(
        &self,
        blob_id: <Self::Share as Share>::BlobId,
        share_idx: <Self::Share as Share>::ShareIndex,
        commitments: <Self::Share as Share>::SharesCommitments,
        light_share: <Self::Share as Share>::LightShare,
    ) -> Result<(), DynError>;

    async fn get_share(
        &self,
        blob_id: <Self::Share as Share>::BlobId,
        share_idx: <Self::Share as Share>::ShareIndex,
    ) -> Result<Option<<Self::Share as Share>::LightShare>, DynError>;

    async fn add_tx(
        &self,
        blob_id: <Self::Share as Share>::BlobId,
        assignations: u16,
        tx: Self::Tx,
    ) -> Result<(), DynError>;

    async fn get_tx(
        &self,
        blob_id: <Self::Share as Share>::BlobId,
    ) -> Result<Option<(u16, Self::Tx)>, DynError>;
}
