use std::{
    collections::HashMap,
    error::Error,
    fmt::{Debug, Display},
    hash::Hash,
};

use lb_core::{
    da::{DaVerifier as CoreDaVerifier, blob::Share},
    header::HeaderId,
    mantle::{
        SignedMantleTx,
        ops::channel::{ChannelId, Ed25519PublicKey, MsgId},
        tx_builder::MantleTxBuilder,
    },
    sdp::SessionNumber,
};
use lb_da_dispersal_service::{
    DaDispersalMsg, DispersalService, adapters::network::DispersalNetworkAdapter,
    backend::DispersalBackend,
};
use lb_da_network_core::{SubnetworkId, maintenance::monitor::ConnectionMonitorCommand};
use lb_da_network_service::{
    DaNetworkMsg, MembershipResponse, NetworkService,
    api::ApiAdapter as ApiAdapterTrait,
    backends::{
        NetworkBackend,
        libp2p::{executor::ExecutorDaNetworkMessage, validator::DaNetworkMessage},
    },
    sdp::SdpAdapter as SdpAdapterTrait,
};
use lb_da_sampling_service::{
    DaSamplingService, DaSamplingServiceMsg, backend::DaSamplingServiceBackend,
    mempool::DaMempoolAdapter as DaMempoolSamplingAdapter,
};
use lb_da_verifier_service::{
    DaVerifierMsg, DaVerifierService, backend::VerifierBackend, mempool::DaMempoolAdapter,
    storage::adapters::rocksdb::RocksAdapter as VerifierStorageAdapter,
};
use lb_kzgrs_backend::common::share::DaSharesCommitments;
use lb_libp2p::PeerId;
use lb_storage_service::{api::da::DaConverter, backends::rocksdb::RocksBackend};
use lb_subnetworks_assignations::MembershipHandler;
use overwatch::{DynError, overwatch::handle::OverwatchHandle, services::AsServiceId};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::oneshot;

pub type DaVerifier<
    Blob,
    NetworkAdapter,
    VerifierBackend,
    DaStorageConverter,
    VerifierMempoolAdapter,
    RuntimeServiceId,
> = DaVerifierService<
    VerifierBackend,
    NetworkAdapter,
    VerifierStorageAdapter<Blob, DaStorageConverter>,
    VerifierMempoolAdapter,
    RuntimeServiceId,
>;

pub type DaDispersal<Backend, NetworkAdapter, Membership, RuntimeServiceId> =
    DispersalService<Backend, NetworkAdapter, Membership, RuntimeServiceId>;

pub type DaNetwork<
    Backend,
    Membership,
    MembershipAdapter,
    StorageAdapter,
    ApiAdapter,
    SdpAdapter,
    RuntimeServiceId,
> = NetworkService<
    Backend,
    Membership,
    MembershipAdapter,
    StorageAdapter,
    ApiAdapter,
    SdpAdapter,
    RuntimeServiceId,
>;

pub async fn add_share<
    DaShare,
    VerifierNetwork,
    ShareVerifier,
    DaStorageConverter,
    VerifierMempoolAdapter,
    RuntimeServiceId,
>(
    handle: &OverwatchHandle<RuntimeServiceId>,
    share: DaShare,
) -> Result<Option<()>, DynError>
where
    DaShare: Share + Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    <DaShare as Share>::BlobId: Clone + Send + Sync + 'static,
    <DaShare as Share>::ShareIndex: Clone + Eq + Hash + Send + Sync + 'static,
    <DaShare as Share>::LightShare: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    <DaShare as Share>::SharesCommitments:
        Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    VerifierNetwork: lb_da_verifier_service::network::NetworkAdapter<RuntimeServiceId>,
    VerifierNetwork::Settings: Clone,
    ShareVerifier: VerifierBackend + CoreDaVerifier<DaShare = DaShare>,
    <ShareVerifier as VerifierBackend>::Settings: Clone,
    <ShareVerifier as CoreDaVerifier>::Error: Error,
    DaStorageConverter:
        DaConverter<RocksBackend, Share = DaShare, Tx = SignedMantleTx> + Send + Sync + 'static,
    VerifierMempoolAdapter: DaMempoolAdapter,
    RuntimeServiceId: Debug
        + Sync
        + Display
        + AsServiceId<
            DaVerifier<
                DaShare,
                VerifierNetwork,
                ShareVerifier,
                DaStorageConverter,
                VerifierMempoolAdapter,
                RuntimeServiceId,
            >,
        >,
{
    let relay = handle.relay().await?;
    let (sender, receiver) = oneshot::channel();
    relay
        .send(DaVerifierMsg::AddShare {
            share,
            reply_channel: sender,
        })
        .await
        .map_err(|(e, _)| e)?;

    receiver
        .await
        .map_err(|_| DynError::from("Failed to add share"))
}

pub async fn get_commitments<
    SamplingBackend,
    SamplingNetwork,
    DaSamplingMempool,
    SamplingStorage,
    RuntimeServiceId,
>(
    handle: &OverwatchHandle<RuntimeServiceId>,
    blob_id: SamplingBackend::BlobId,
    session: SessionNumber,
) -> Result<Option<DaSharesCommitments>, DynError>
where
    SamplingBackend: DaSamplingServiceBackend,
    <SamplingBackend as DaSamplingServiceBackend>::BlobId: Send + 'static,
    SamplingNetwork: lb_da_sampling_service::network::NetworkAdapter<RuntimeServiceId>,
    SamplingStorage: lb_da_sampling_service::storage::DaStorageAdapter<RuntimeServiceId>,
    DaSamplingMempool: DaMempoolSamplingAdapter,
    RuntimeServiceId: Debug
        + Sync
        + Display
        + AsServiceId<
            DaSamplingService<
                SamplingBackend,
                SamplingNetwork,
                SamplingStorage,
                DaSamplingMempool,
                RuntimeServiceId,
            >,
        >,
{
    let relay = handle.relay().await?;
    let (sender, receiver) = oneshot::channel();
    relay
        .send(DaSamplingServiceMsg::GetCommitments {
            blob_id,
            session,
            response_sender: sender,
        })
        .await
        .map_err(|(e, _)| e)?;

    receiver
        .await
        .map_err(|_| DynError::from("Failed to get range"))
}

pub async fn disperse_data<Backend, NetworkAdapter, Membership, RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
    channel_id: ChannelId,
    parent_msg_id: MsgId,
    signer: Ed25519PublicKey,
    data: Vec<u8>,
) -> Result<Backend::BlobId, DynError>
where
    Membership: MembershipHandler<NetworkId = SubnetworkId, Id = PeerId>
        + Clone
        + Debug
        + Send
        + Sync
        + 'static,
    Backend: DispersalBackend<NetworkAdapter = NetworkAdapter> + Send + Sync + 'static,
    Backend::Settings: Clone + Send + Sync,
    Backend::BlobId: Serialize,
    NetworkAdapter: DispersalNetworkAdapter<SubnetworkId = Membership::NetworkId> + Send,
    RuntimeServiceId: Debug
        + Send
        + Sync
        + Display
        + AsServiceId<DaDispersal<Backend, NetworkAdapter, Membership, RuntimeServiceId>>
        + 'static,
{
    // TODO: Should tx_builder come from wallet service?
    // Provide proper tx_builder when DA uses actual wallet instead of mock.
    let tx_builder = MantleTxBuilder::new();

    let relay = handle.relay().await?;
    let (sender, receiver) = oneshot::channel();
    relay
        .send(DaDispersalMsg::Disperse {
            tx_builder,
            channel_id,
            parent_msg_id,
            signer,
            data,
            reply_channel: sender,
        })
        .await
        .map_err(|(e, _)| e)?;

    receiver
        .await
        .map_err(|_| DynError::from("Failed to disperse data"))?
}

pub async fn block_peer<
    Backend,
    Membership,
    MembershipAdapter,
    StorageAdapter,
    ApiAdapter,
    SdpAdapter,
    RuntimeServiceId,
>(
    handle: &OverwatchHandle<RuntimeServiceId>,
    peer_id: PeerId,
) -> Result<bool, DynError>
where
    Backend: NetworkBackend<RuntimeServiceId> + 'static + Send,
    Backend::Message: MonitorMessageFactory,
    Membership: MembershipHandler + Clone + Send + Sync + 'static,
    Membership::Id: Send + Sync + 'static,
    Membership::NetworkId: Send + Sync + 'static,
    ApiAdapter: ApiAdapterTrait + Send + Sync + 'static,
    SdpAdapter: SdpAdapterTrait<RuntimeServiceId> + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + Sync
        + Display
        + 'static
        + AsServiceId<
            NetworkService<
                Backend,
                Membership,
                MembershipAdapter,
                StorageAdapter,
                ApiAdapter,
                SdpAdapter,
                RuntimeServiceId,
            >,
        >,
{
    let relay = handle.relay().await?;
    let (sender, receiver) = oneshot::channel();
    let message = Backend::Message::create_block_message(peer_id, sender);
    relay
        .send(DaNetworkMsg::Process(message))
        .await
        .map_err(|(e, _)| e)?;

    receiver
        .await
        .map_err(|_| DynError::from("Failed to block peer"))
}

pub async fn unblock_peer<
    Backend,
    Membership,
    MembershipAdapter,
    StorageAdapter,
    ApiAdapter,
    SdpAdapter,
    RuntimeServiceId,
>(
    handle: &OverwatchHandle<RuntimeServiceId>,
    peer_id: PeerId,
) -> Result<bool, DynError>
where
    Backend: NetworkBackend<RuntimeServiceId> + 'static + Send,
    Backend::Message: MonitorMessageFactory,
    Membership: MembershipHandler + Clone + Send + Sync + 'static,
    Membership::Id: Send + Sync + 'static,
    Membership::NetworkId: Send + Sync + 'static,
    ApiAdapter: ApiAdapterTrait + Send + Sync + 'static,
    SdpAdapter: SdpAdapterTrait<RuntimeServiceId> + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + Sync
        + Display
        + 'static
        + AsServiceId<
            NetworkService<
                Backend,
                Membership,
                MembershipAdapter,
                StorageAdapter,
                ApiAdapter,
                SdpAdapter,
                RuntimeServiceId,
            >,
        >,
{
    let relay = handle.relay().await?;
    let (sender, receiver) = oneshot::channel();
    let message = Backend::Message::create_unblock_message(peer_id, sender);
    relay
        .send(DaNetworkMsg::Process(message))
        .await
        .map_err(|(e, _)| e)?;

    receiver
        .await
        .map_err(|_| DynError::from("Failed to unblock peer"))
}

pub async fn blacklisted_peers<
    Backend,
    Membership,
    MembershipAdapter,
    StorageAdapter,
    ApiAdapter,
    SdpAdapter,
    RuntimeServiceId,
>(
    handle: &OverwatchHandle<RuntimeServiceId>,
) -> Result<Vec<PeerId>, DynError>
where
    Backend: NetworkBackend<RuntimeServiceId> + 'static + Send,
    Backend::Message: MonitorMessageFactory,
    Membership: MembershipHandler + Clone + Send + Sync + 'static,
    Membership::Id: Send + Sync + 'static,
    Membership::NetworkId: Send + Sync + 'static,
    ApiAdapter: ApiAdapterTrait + Send + Sync + 'static,
    SdpAdapter: SdpAdapterTrait<RuntimeServiceId> + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + Sync
        + Display
        + 'static
        + AsServiceId<
            NetworkService<
                Backend,
                Membership,
                MembershipAdapter,
                StorageAdapter,
                ApiAdapter,
                SdpAdapter,
                RuntimeServiceId,
            >,
        >,
{
    let relay = handle.relay().await?;
    let (sender, receiver) = oneshot::channel();
    let message = Backend::Message::create_blacklisted_message(sender);
    relay
        .send(DaNetworkMsg::Process(message))
        .await
        .map_err(|(e, _)| e)?;

    receiver
        .await
        .map_err(|_| DynError::from("Failed to get blacklisted peers"))
}

pub async fn da_get_membership<
    Backend,
    Membership,
    MembershipAdapter,
    StorageAdapter,
    ApiAdapter,
    SdpAdapter,
    RuntimeServiceId,
>(
    handle: OverwatchHandle<RuntimeServiceId>,
    session_id: SessionNumber,
) -> Result<MembershipResponse, DynError>
where
    Backend: NetworkBackend<RuntimeServiceId> + 'static + Send,
    Membership: MembershipHandler + Clone + Send + Sync + 'static,
    Membership::Id: Send + Sync + 'static,
    Membership::NetworkId: Send + Sync + 'static,
    ApiAdapter: ApiAdapterTrait + Send + Sync + 'static,
    SdpAdapter: SdpAdapterTrait<RuntimeServiceId> + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + Sync
        + Display
        + 'static
        + AsServiceId<
            NetworkService<
                Backend,
                Membership,
                MembershipAdapter,
                StorageAdapter,
                ApiAdapter,
                SdpAdapter,
                RuntimeServiceId,
            >,
        >,
{
    let relay = handle.relay().await?;
    let (sender, receiver) = oneshot::channel();
    let message = DaNetworkMsg::GetMembership { session_id, sender };
    relay.send(message).await.map_err(|(e, _)| e)?;

    receiver
        .await
        .map_err(|_| DynError::from("Failed to get membership"))
}

#[expect(clippy::implicit_hasher, reason = "we don't need custom hashers")]
pub async fn da_historic_sampling<
    SamplingBackend,
    SamplingNetwork,
    SamplingStorage,
    DaSamplingMempool,
    RuntimeServiceId,
>(
    handle: OverwatchHandle<RuntimeServiceId>,
    block_id: HeaderId,
    blob_ids: HashMap<SamplingBackend::BlobId, SessionNumber>,
) -> Result<bool, DynError>
where
    SamplingBackend: DaSamplingServiceBackend,
    <SamplingBackend as DaSamplingServiceBackend>::BlobId: Send + Eq + Hash + 'static,
    SamplingNetwork: lb_da_sampling_service::network::NetworkAdapter<RuntimeServiceId>,
    SamplingStorage: lb_da_sampling_service::storage::DaStorageAdapter<RuntimeServiceId>,
    DaSamplingMempool: DaMempoolSamplingAdapter,
    RuntimeServiceId: Debug
        + Sync
        + Display
        + AsServiceId<
            DaSamplingService<
                SamplingBackend,
                SamplingNetwork,
                SamplingStorage,
                DaSamplingMempool,
                RuntimeServiceId,
            >,
        >,
{
    let relay = handle.relay().await?;
    let (sender, receiver) = oneshot::channel();

    let message = DaSamplingServiceMsg::RequestHistoricSampling {
        block_id,
        blob_ids,
        reply_channel: sender,
    };
    relay.send(message).await.map_err(|(e, _)| e)?;

    receiver
        .await
        .map_err(|_| DynError::from("Failed to get historic sampling"))
}

pub async fn balancer_stats<
    Backend,
    Membership,
    MembershipAdapter,
    StorageAdapter,
    ApiAdapter,
    SdpAdapter,
    RuntimeServiceId,
>(
    handle: &OverwatchHandle<RuntimeServiceId>,
) -> Result<<Backend::Message as BalancerMessageFactory>::BalancerStats, DynError>
where
    Backend: NetworkBackend<RuntimeServiceId> + 'static + Send,
    Backend::Message: BalancerMessageFactory,
    Membership: MembershipHandler + Clone + Send + Sync + 'static,
    Membership::Id: Send + Sync + 'static,
    Membership::NetworkId: Send + Sync + 'static,
    ApiAdapter: ApiAdapterTrait + Send + Sync + 'static,
    SdpAdapter: SdpAdapterTrait<RuntimeServiceId> + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + Sync
        + Display
        + 'static
        + AsServiceId<
            NetworkService<
                Backend,
                Membership,
                MembershipAdapter,
                StorageAdapter,
                ApiAdapter,
                SdpAdapter,
                RuntimeServiceId,
            >,
        >,
{
    let relay = handle.relay().await?;
    let (sender, receiver) = oneshot::channel();
    let message = Backend::Message::create_stats_message(sender);
    relay
        .send(DaNetworkMsg::Process(message))
        .await
        .map_err(|(e, _)| e)?;

    receiver
        .await
        .map_err(|_| DynError::from("Failed to get balancer stats"))
}

pub async fn monitor_stats<
    Backend,
    Membership,
    MembershipAdapter,
    StorageAdapter,
    ApiAdapter,
    SdpAdapter,
    RuntimeServiceId,
>(
    handle: &OverwatchHandle<RuntimeServiceId>,
) -> Result<<Backend::Message as MonitorMessageFactory>::MonitorStats, DynError>
where
    Backend: NetworkBackend<RuntimeServiceId> + 'static + Send,
    Backend::Message: MonitorMessageFactory,
    Membership: MembershipHandler + Clone + Send + Sync + 'static,
    Membership::Id: Send + Sync + 'static,
    Membership::NetworkId: Send + Sync + 'static,
    ApiAdapter: ApiAdapterTrait + Send + Sync + 'static,
    SdpAdapter: SdpAdapterTrait<RuntimeServiceId> + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + Sync
        + Display
        + 'static
        + AsServiceId<
            NetworkService<
                Backend,
                Membership,
                MembershipAdapter,
                StorageAdapter,
                ApiAdapter,
                SdpAdapter,
                RuntimeServiceId,
            >,
        >,
{
    let relay = handle.relay().await?;
    let (sender, receiver) = oneshot::channel();
    let message = Backend::Message::create_stats_message(sender);
    relay
        .send(DaNetworkMsg::Process(message))
        .await
        .map_err(|(e, _)| e)?;

    receiver
        .await
        .map_err(|_| DynError::from("Failed to get monitor stats"))
}

// Factory for generating messages for connection monitor (validator and
// executor).
pub trait MonitorMessageFactory {
    type MonitorStats: Debug + Serialize;

    fn create_block_message(peer_id: PeerId, sender: oneshot::Sender<bool>) -> Self;
    fn create_unblock_message(peer_id: PeerId, sender: oneshot::Sender<bool>) -> Self;
    fn create_blacklisted_message(sender: oneshot::Sender<Vec<PeerId>>) -> Self;
    fn create_stats_message(sender: oneshot::Sender<Self::MonitorStats>) -> Self;
}

impl<BalancerStats, MonitorStats> MonitorMessageFactory
    for DaNetworkMessage<BalancerStats, MonitorStats>
where
    BalancerStats: Debug + Serialize,
    MonitorStats: Debug + Serialize,
{
    type MonitorStats = MonitorStats;

    fn create_block_message(peer_id: PeerId, sender: oneshot::Sender<bool>) -> Self {
        Self::MonitorRequest(ConnectionMonitorCommand::Block(peer_id, sender))
    }

    fn create_unblock_message(peer_id: PeerId, sender: oneshot::Sender<bool>) -> Self {
        Self::MonitorRequest(ConnectionMonitorCommand::Unblock(peer_id, sender))
    }

    fn create_blacklisted_message(sender: oneshot::Sender<Vec<PeerId>>) -> Self {
        Self::MonitorRequest(ConnectionMonitorCommand::BlacklistedPeers(sender))
    }

    fn create_stats_message(sender: oneshot::Sender<Self::MonitorStats>) -> Self {
        Self::MonitorRequest(ConnectionMonitorCommand::Stats(sender))
    }
}

impl<BalancerStats, MonitorStats> MonitorMessageFactory
    for ExecutorDaNetworkMessage<BalancerStats, MonitorStats>
where
    BalancerStats: Debug + Serialize,
    MonitorStats: Debug + Serialize,
{
    type MonitorStats = MonitorStats;

    fn create_block_message(peer_id: PeerId, sender: oneshot::Sender<bool>) -> Self {
        Self::MonitorRequest(ConnectionMonitorCommand::Block(peer_id, sender))
    }

    fn create_unblock_message(peer_id: PeerId, sender: oneshot::Sender<bool>) -> Self {
        Self::MonitorRequest(ConnectionMonitorCommand::Unblock(peer_id, sender))
    }

    fn create_blacklisted_message(sender: oneshot::Sender<Vec<PeerId>>) -> Self {
        Self::MonitorRequest(ConnectionMonitorCommand::BlacklistedPeers(sender))
    }

    fn create_stats_message(sender: oneshot::Sender<Self::MonitorStats>) -> Self {
        Self::MonitorRequest(ConnectionMonitorCommand::Stats(sender))
    }
}

pub trait BalancerMessageFactory {
    type BalancerStats: Debug + Serialize;

    fn create_stats_message(sender: oneshot::Sender<Self::BalancerStats>) -> Self;
}

impl<BalancerStats, MonitorStats> BalancerMessageFactory
    for DaNetworkMessage<BalancerStats, MonitorStats>
where
    BalancerStats: Debug + Serialize,
{
    type BalancerStats = BalancerStats;

    fn create_stats_message(sender: oneshot::Sender<BalancerStats>) -> Self {
        Self::BalancerStats(sender)
    }
}

impl<BalancerStats, MonitorStats> BalancerMessageFactory
    for ExecutorDaNetworkMessage<BalancerStats, MonitorStats>
where
    BalancerStats: Debug + Serialize,
{
    type BalancerStats = BalancerStats;

    fn create_stats_message(sender: oneshot::Sender<BalancerStats>) -> Self {
        Self::BalancerStats(sender)
    }
}
