use std::{
    collections::{HashMap, HashSet},
    error::Error,
};

use async_trait::async_trait;
use multiaddr::Multiaddr;
use nomos_core::{
    da::blob::Share,
    sdp::{ProviderId, SessionNumber},
};

pub mod requests;

type ServiceBlobId<Converter, Backend> =
    <<Converter as DaConverter<Backend>>::Share as Share>::BlobId;

type ServiceShareIndex<Converter, Backend> =
    <<Converter as DaConverter<Backend>>::Share as Share>::ShareIndex;

type ServiceLightShare<Converter, Backend> =
    <<Converter as DaConverter<Backend>>::Share as Share>::LightShare;

type ServiceSharedCommitments<Converter, Backend> =
    <<Converter as DaConverter<Backend>>::Share as Share>::SharesCommitments;

type ServiceTx<Converter, Backend> = <Converter as DaConverter<Backend>>::Tx;

pub trait DaConverter<Backend: StorageDaApi> {
    type Share: Share;
    type Tx;
    type Error: Error + Send + Sync + 'static;

    fn blob_id_to_storage(
        blob_id: ServiceBlobId<Self, Backend>,
    ) -> Result<Backend::BlobId, Self::Error>;

    fn blob_id_from_storage(
        blob_id: Backend::BlobId,
    ) -> Result<ServiceBlobId<Self, Backend>, Self::Error>;

    fn share_index_to_storage(
        share_index: ServiceShareIndex<Self, Backend>,
    ) -> Result<Backend::ShareIndex, Self::Error>;

    fn share_index_from_storage(
        share_index: Backend::ShareIndex,
    ) -> Result<ServiceShareIndex<Self, Backend>, Self::Error>;

    fn share_to_storage(
        service_share: ServiceLightShare<Self, Backend>,
    ) -> Result<Backend::Share, Self::Error>;

    fn share_from_storage(
        backend_share: Backend::Share,
    ) -> Result<ServiceLightShare<Self, Backend>, Self::Error>;

    fn commitments_to_storage(
        service_commitments: ServiceSharedCommitments<Self, Backend>,
    ) -> Result<Backend::Commitments, Self::Error>;

    fn commitments_from_storage(
        backend_commitments: Backend::Commitments,
    ) -> Result<ServiceSharedCommitments<Self, Backend>, Self::Error>;

    fn tx_to_storage(service_tx: ServiceTx<Self, Backend>) -> Result<Backend::Tx, Self::Error>;

    fn tx_from_storage(backend_tx: Backend::Tx) -> Result<ServiceTx<Self, Backend>, Self::Error>;
}

#[async_trait]
pub trait StorageDaApi {
    type Error: Error + Send + Sync + 'static;
    type BlobId: Send + Sync;
    type Share: Send + Sync;
    type Commitments: Send + Sync;
    type ShareIndex: Send + Sync;
    type Id: Send + Sync;
    type NetworkId: Send + Sync;
    type Tx: Send + Sync;

    async fn get_light_share(
        &mut self,
        blob_id: Self::BlobId,
        share_idx: Self::ShareIndex,
    ) -> Result<Option<Self::Share>, Self::Error>;

    async fn get_blob_light_shares(
        &mut self,
        blob_id: Self::BlobId,
    ) -> Result<Option<Vec<Self::Share>>, Self::Error>;

    async fn get_blob_share_indices(
        &mut self,
        blob_id: Self::BlobId,
    ) -> Result<Option<HashSet<Self::ShareIndex>>, Self::Error>;

    async fn store_light_share(
        &mut self,
        blob_id: Self::BlobId,
        share_idx: Self::ShareIndex,
        light_share: Self::Share,
    ) -> Result<(), Self::Error>;

    async fn get_shared_commitments(
        &mut self,
        blob_id: Self::BlobId,
    ) -> Result<Option<Self::Commitments>, Self::Error>;

    async fn store_shared_commitments(
        &mut self,
        blob_id: Self::BlobId,
        shared_commitments: Self::Commitments,
    ) -> Result<(), Self::Error>;

    async fn store_assignations(
        &mut self,
        session_id: SessionNumber,
        assignations: HashMap<Self::NetworkId, HashSet<Self::Id>>,
    ) -> Result<(), Self::Error>;

    async fn prune_assignations(
        &mut self,
        cutoff_session: SessionNumber,
    ) -> Result<(), Self::Error>;

    async fn store_providerid_mappings(
        &mut self,
        mappings: HashMap<Self::Id, ProviderId>,
    ) -> Result<(), Self::Error>;

    async fn get_assignations(
        &mut self,
        session_id: SessionNumber,
    ) -> Result<Option<HashMap<Self::NetworkId, HashSet<Self::Id>>>, Self::Error>;

    async fn get_provider_id(&mut self, id: Self::Id) -> Result<Option<ProviderId>, Self::Error>;

    async fn store_addresses(
        &mut self,
        ids: HashMap<Self::Id, Multiaddr>,
    ) -> Result<(), Self::Error>;

    async fn get_address(&mut self, id: Self::Id) -> Result<Option<Multiaddr>, Self::Error>;

    async fn store_tx(
        &mut self,
        blob_id: Self::BlobId,
        assignations: u16,
        tx: Self::Tx,
    ) -> Result<(), Self::Error>;

    async fn get_tx(
        &mut self,
        blob_id: Self::BlobId,
    ) -> Result<Option<(u16, Self::Tx)>, Self::Error>;
}
