use std::{collections::HashSet, fmt::Debug, marker::PhantomData, pin::Pin, time::Duration};

use futures::{Stream, StreamExt as _, stream::BoxStream};
use kzgrs_backend::common::share::{DaShare, DaSharesCommitments};
use nomos_core::{da::BlobId, mantle::SignedMantleTx, sdp::SessionNumber};
use nomos_da_network_core::{
    PeerId, SubnetworkId,
    protocols::{
        dispersal::executor::behaviour::DispersalExecutorEvent, sampling::errors::SamplingError,
    },
};
use nomos_da_network_service::{
    DaNetworkMsg, NetworkService,
    api::ApiAdapter as ApiAdapterTrait,
    backends::libp2p::{
        common::SamplingEvent,
        executor::{
            DaNetworkEvent, DaNetworkEventKind, DaNetworkExecutorBackend, ExecutorDaNetworkMessage,
        },
    },
    membership::MembershipAdapter,
    sdp::SdpAdapter as SdpAdapterTrait,
};
use overwatch::{
    DynError,
    services::{ServiceData, relay::OutboundRelay},
};
use subnetworks_assignations::MembershipHandler;
use tokio::sync::oneshot;

use crate::adapters::network::DispersalNetworkAdapter;

pub struct Libp2pNetworkAdapter<
    Membership,
    MembershipServiceAdapter,
    StorageAdapter,
    ApiAdapter,
    SdpAdapter,
    RuntimeServiceId,
> where
    Membership: MembershipHandler<NetworkId = SubnetworkId, Id = PeerId>
        + Clone
        + Debug
        + Send
        + Sync
        + 'static,
    MembershipServiceAdapter: MembershipAdapter,
    ApiAdapter: ApiAdapterTrait,
    SdpAdapter: SdpAdapterTrait<RuntimeServiceId>,
{
    outbound_relay: OutboundRelay<
        DaNetworkMsg<DaNetworkExecutorBackend<Membership>, DaSharesCommitments, RuntimeServiceId>,
    >,
    _phantom: PhantomData<(
        RuntimeServiceId,
        MembershipServiceAdapter,
        StorageAdapter,
        ApiAdapter,
        SdpAdapter,
    )>,
}

impl<Membership, MembershipServiceAdapter, StorageAdapter, ApiAdapter, SdpAdapter, RuntimeServiceId>
    Libp2pNetworkAdapter<
        Membership,
        MembershipServiceAdapter,
        StorageAdapter,
        ApiAdapter,
        SdpAdapter,
        RuntimeServiceId,
    >
where
    Membership: MembershipHandler<NetworkId = SubnetworkId, Id = PeerId>
        + Clone
        + Debug
        + Send
        + Sync
        + 'static,
    MembershipServiceAdapter: MembershipAdapter + Sync,
    ApiAdapter: ApiAdapterTrait + Sync,
    SdpAdapter: SdpAdapterTrait<RuntimeServiceId> + Sync,
    StorageAdapter: Sync,
    RuntimeServiceId: Sync,
{
    async fn start_sampling(
        &self,
        blob_id: BlobId,
        session: SessionNumber,
    ) -> Result<(), DynError> {
        self.outbound_relay
            .send(DaNetworkMsg::Process(
                ExecutorDaNetworkMessage::RequestSample { blob_id, session },
            ))
            .await
            .expect("RequestSample message should have been sent");
        Ok(())
    }
}

#[async_trait::async_trait]
impl<Membership, MembershipServiceAdapter, StorageAdapter, ApiAdapter, SdpAdapter, RuntimeServiceId>
    DispersalNetworkAdapter
    for Libp2pNetworkAdapter<
        Membership,
        MembershipServiceAdapter,
        StorageAdapter,
        ApiAdapter,
        SdpAdapter,
        RuntimeServiceId,
    >
where
    Membership: MembershipHandler<NetworkId = SubnetworkId, Id = PeerId>
        + Clone
        + Debug
        + Send
        + Sync
        + 'static,
    MembershipServiceAdapter: MembershipAdapter + Sync,
    ApiAdapter: ApiAdapterTrait + Sync,
    SdpAdapter: SdpAdapterTrait<RuntimeServiceId> + Sync,
    StorageAdapter: Sync,
    RuntimeServiceId: Sync,
{
    type NetworkService = NetworkService<
        DaNetworkExecutorBackend<Membership>,
        Membership,
        MembershipServiceAdapter,
        StorageAdapter,
        ApiAdapter,
        SdpAdapter,
        RuntimeServiceId,
    >;

    type SubnetworkId = Membership::NetworkId;

    fn new(outbound_relay: OutboundRelay<<Self::NetworkService as ServiceData>::Message>) -> Self {
        Self {
            outbound_relay,
            _phantom: PhantomData,
        }
    }

    async fn disperse_share(
        &self,
        subnetwork_id: Self::SubnetworkId,
        da_share: DaShare,
    ) -> Result<(), DynError> {
        let (sender, rx) = oneshot::channel();
        self.outbound_relay
            .send(DaNetworkMsg::Process(
                ExecutorDaNetworkMessage::RequestShareDispersal {
                    subnetwork_id,
                    da_share: Box::new(da_share),
                    sender,
                },
            ))
            .await
            .map_err(|(e, _)| Box::new(e) as DynError)?;
        rx.await?.map_err(|e| Box::new(e) as DynError)
    }

    async fn disperse_tx(
        &self,
        subnetwork_id: Self::SubnetworkId,
        tx: SignedMantleTx,
    ) -> Result<(), DynError> {
        let (sender, rx) = oneshot::channel();
        self.outbound_relay
            .send(DaNetworkMsg::Process(
                ExecutorDaNetworkMessage::RequestTxDispersal {
                    subnetwork_id,
                    tx: Box::new(tx),
                    sender,
                },
            ))
            .await
            .map_err(|(e, _)| Box::new(e) as DynError)?;
        rx.await?.map_err(|e| Box::new(e) as DynError)
    }

    async fn dispersal_events_stream(
        &self,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<(BlobId, Self::SubnetworkId), DynError>> + Send>>,
        DynError,
    > {
        let (sender, receiver) = oneshot::channel();
        self.outbound_relay
            .send(DaNetworkMsg::Subscribe {
                kind: DaNetworkEventKind::Dispersal,
                sender,
            })
            .await
            .map_err(|(e, _)| Box::new(e) as DynError)?;
        receiver
            .await
            .map_err(|e| Box::new(e) as DynError)
            .map(|stream| {
                Box::pin(stream.filter_map(async |event| match event {
                    DaNetworkEvent::Sampling(_)
                    | DaNetworkEvent::Commitments(_)
                    | DaNetworkEvent::Verifying(_)
                    | DaNetworkEvent::HistoricSampling(_) => None,
                    DaNetworkEvent::Dispersal(DispersalExecutorEvent::DispersalError { error }) => {
                        Some(Err(Box::new(error) as DynError))
                    }
                    DaNetworkEvent::Dispersal(DispersalExecutorEvent::DispersalSuccess {
                        blob_id,
                        subnetwork_id,
                    }) => Some(Ok((blob_id, subnetwork_id))),
                }))
                    as BoxStream<'static, Result<(BlobId, Self::SubnetworkId), DynError>>
            })
    }

    async fn get_blob_samples(
        &self,
        blob_id: BlobId,
        session: SessionNumber,
        subnets: &[SubnetworkId],
        cooldown: Duration,
    ) -> Result<(), DynError> {
        enum SampleOutcome {
            Success(u16),
            Retry(u16),
        }

        let expected_count = subnets.len();
        let mut success_count = 0;

        let mut pending_subnets: HashSet<SubnetworkId> = subnets.iter().copied().collect();

        let (stream_sender, stream_receiver) = oneshot::channel();

        self.outbound_relay
            .send(DaNetworkMsg::Subscribe {
                kind: DaNetworkEventKind::Sampling,
                sender: stream_sender,
            })
            .await
            .map_err(|(error, _)| error)?;

        self.start_sampling(blob_id, session).await?;

        let stream = stream_receiver.await.map_err(Box::new)?;

        let mut stream = tokio_stream::StreamExt::filter_map(stream, move |event| match event {
            DaNetworkEvent::Sampling(event) if event.has_blob_id(&blob_id) => match event {
                SamplingEvent::SamplingSuccess { light_share, .. } => {
                    Some(SampleOutcome::Success(light_share.share_idx))
                }
                SamplingEvent::SamplingError { error } => match error {
                    SamplingError::Share { subnetwork_id, .. }
                    | SamplingError::Deserialize { subnetwork_id, .. }
                    | SamplingError::BlobNotFound { subnetwork_id, .. } => {
                        Some(SampleOutcome::Retry(subnetwork_id))
                    }
                    _ => None,
                },
                SamplingEvent::SamplingRequest { .. } => None,
            },
            _ => None,
        });

        loop {
            tokio::select! {
                Some(event) = stream.next() => {
                    match event {
                        SampleOutcome::Success(subnetwork_id) => {
                            success_count += 1;
                            pending_subnets.remove(&subnetwork_id);
                            if success_count >= expected_count {
                                return Ok(());
                            }
                        }
                        SampleOutcome::Retry(subnetwork_id) => {
                            pending_subnets.insert(subnetwork_id);
                        }
                    }
                }
                () = tokio::time::sleep(cooldown) => {
                    if !pending_subnets.is_empty() {
                        self.start_sampling(blob_id, session).await?;
                    }
                }
            }
        }
    }
}
