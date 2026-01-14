pub mod adapters;

use std::{collections::HashMap, pin::Pin};

use futures::Stream;
use nomos_core::{da::BlobId, header::HeaderId, sdp::SessionNumber};
use nomos_da_network_service::{
    NetworkService,
    api::ApiAdapter,
    backends::{
        NetworkBackend,
        libp2p::common::{CommitmentsEvent, HistoricSamplingEvent, SamplingEvent},
    },
    sdp::SdpAdapter,
};
use overwatch::{
    DynError,
    services::{ServiceData, relay::OutboundRelay},
};
use subnetworks_assignations::MembershipHandler;

#[async_trait::async_trait]
pub trait NetworkAdapter<RuntimeServiceId> {
    type Backend: NetworkBackend<RuntimeServiceId> + Send + 'static;
    type Settings: Clone;
    type Membership: MembershipHandler;
    type Storage;
    type MembershipAdapter;
    type ApiAdapter: ApiAdapter;
    type SdpAdapter: SdpAdapter<RuntimeServiceId>;

    async fn new(
        network_relay: OutboundRelay<
            <NetworkService<
                Self::Backend,
                Self::Membership,
                Self::MembershipAdapter,
                Self::Storage,
                Self::ApiAdapter,
                Self::SdpAdapter,
                RuntimeServiceId,
            > as ServiceData>::Message,
        >,
    ) -> Self;

    async fn start_sampling(
        &mut self,
        blob_id: BlobId,
        session: SessionNumber,
    ) -> Result<(), DynError>;

    async fn request_historic_sampling(
        &self,
        block_id: HeaderId,
        blob_ids: HashMap<BlobId, SessionNumber>,
    ) -> Result<(), DynError>;

    async fn request_historic_commitments(
        &self,
        block_id: HeaderId,
        blob_id: BlobId,
        session: SessionNumber,
    ) -> Result<(), DynError>;

    async fn listen_to_sampling_messages(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = SamplingEvent> + Send>>, DynError>;

    async fn request_commitments(
        &self,
        blob_id: BlobId,
        session: SessionNumber,
    ) -> Result<(), DynError>;

    async fn listen_to_commitments_messages(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = CommitmentsEvent> + Send>>, DynError>;

    async fn listen_to_historic_sampling_messages(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = HistoricSamplingEvent> + Send>>, DynError>;
}
