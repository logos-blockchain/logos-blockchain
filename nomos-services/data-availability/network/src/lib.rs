pub mod addressbook;
pub mod api;
pub mod backends;
pub mod membership;
mod opinion_aggregator;
pub mod sdp;
pub mod storage;

use std::{
    collections::{HashMap, HashSet},
    fmt::{self, Debug, Display},
    marker::PhantomData,
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use backends::{ConnectionStatus, NetworkBackend};
use futures::Stream;
use kzgrs_backend::common::share::{DaShare, DaSharesCommitments};
use libp2p::{Multiaddr, PeerId};
use nomos_core::{
    da::BlobId,
    header::HeaderId,
    sdp::{ProviderId, SessionNumber},
};
use nomos_da_network_core::{
    SubnetworkId, addressbook::AddressBookHandler as _,
    protocols::sampling::opinions::OpinionEvent, swarm::BalancerStats,
};
use nomos_libp2p::cryptarchia_sync::DynError;
use overwatch::{
    OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        state::{NoOperator, ServiceState},
    },
};
use serde::{Deserialize, Serialize};
use services_utils::wait_until_services_are_ready;
use storage::{MembershipStorage, MembershipStorageAdapter};
use subnetworks_assignations::{MembershipCreator, MembershipHandler, SubnetworkAssignations};
use tokio::sync::{
    broadcast,
    broadcast::Sender,
    mpsc::{self},
    oneshot,
};
use tokio_stream::{StreamExt as _, wrappers::UnboundedReceiverStream};

use crate::{
    addressbook::{AddressBook, AddressBookSnapshot},
    api::ApiAdapter as ApiAdapterTrait,
    membership::{
        MembershipAdapter, SubnetworkPeers,
        handler::{DaMembershipHandler, SharedMembershipHandler},
    },
    opinion_aggregator::{OpinionAggregator, OpinionError},
    sdp::SdpAdapter as SdpAdapterTrait,
};

pub type DaAddressbook = AddressBook;

// todo: export as config param when deployment config is merged
const ASSIGNATIONS_PRUNE_WINDOW: u64 = 8;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MembershipResponse {
    pub assignations: SubnetworkAssignations<SubnetworkId, PeerId>,
    pub addressbook: AddressBookSnapshot<PeerId>,
}

#[derive(Debug)]
pub enum SessionStatus {
    SufficientMembers,
    InsufficientMembers,
}

pub enum DaNetworkMsg<Backend, Commitments, RuntimeServiceId>
where
    Backend: NetworkBackend<RuntimeServiceId>,
{
    Process(Backend::Message),
    Subscribe {
        kind: Backend::EventKind,
        sender: oneshot::Sender<Pin<Box<dyn Stream<Item = Backend::NetworkEvent> + Send>>>,
    },
    GetMembership {
        session_id: SessionNumber,
        sender: oneshot::Sender<MembershipResponse>,
    },
    GetCommitments {
        blob_id: BlobId,
        sender: oneshot::Sender<Option<Commitments>>,
    },
    RequestHistoricSampling {
        block_id: HeaderId,
        blob_ids: HashMap<BlobId, SessionNumber>,
    },
    RequestHistoricCommitments {
        block_id: HeaderId,
        blob_id: BlobId,
        session: SessionNumber,
    },
}

impl<Backend, Commitments, RuntimeServiceId> Debug
    for DaNetworkMsg<Backend, Commitments, RuntimeServiceId>
where
    Backend: NetworkBackend<RuntimeServiceId>,
{
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Process(msg) => write!(fmt, "DaNetworkMsg::Process({msg:?})"),
            Self::Subscribe { kind, .. } => {
                write!(fmt, "DaNetworkMsg::Subscribe{{ kind: {kind:?}}}")
            }
            Self::GetMembership { session_id, .. } => {
                write!(
                    fmt,
                    "DaNetworkMsg::GetMembership{{ session_id: {session_id} }}"
                )
            }
            Self::GetCommitments { blob_id, .. } => {
                write!(
                    fmt,
                    "DaNetworkMsg::GetCommitments{{ blob_id: {blob_id:?} }}"
                )
            }
            Self::RequestHistoricSampling { blob_ids, block_id } => {
                write!(
                    fmt,
                    "DaNetworkMsg::RequestHistoricSample{{blob_ids: {blob_ids:?}, block_id: {block_id} }}"
                )
            }
            Self::RequestHistoricCommitments {
                block_id,
                blob_id,
                session,
            } => {
                write!(
                    fmt,
                    "DaNwetworkMsg::RequestHistoricCommitments{{block_id: {block_id}, blob_id: {blob_id:?}, session: {session} }}
                    "
                )
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct NetworkConfig<
    Backend: NetworkBackend<RuntimeServiceId>,
    Membership,
    ApiAdapterSettings,
    RuntimeServiceId,
> {
    pub backend: Backend::Settings,
    pub membership: Membership,
    pub api_adapter_settings: ApiAdapterSettings,
    pub subnet_refresh_interval: Duration,
    // Number of connected subnetworks that the node requires to have in order to operate
    // correctly.
    pub subnet_threshold: usize,
    // A minimal number of members during a DA session. If session has less than a minimum members,
    // then DA network service stops processing any incoming and outgoing messages.
    pub min_session_members: usize,
}

impl<Backend: NetworkBackend<RuntimeServiceId>, Membership, ApiAdapterSettings, RuntimeServiceId>
    Debug for NetworkConfig<Backend, Membership, ApiAdapterSettings, RuntimeServiceId>
where
    Membership: Clone + Debug,
{
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "NetworkConfig {{ backend: {:?}}}", self.backend)
    }
}

pub struct NetworkService<
    Backend,
    Membership,
    MembershipServiceAdapter,
    StorageAdapter,
    ApiAdapter,
    SdpAdapter,
    RuntimeServiceId,
> where
    Backend: NetworkBackend<RuntimeServiceId> + Send + 'static,
    Membership: MembershipHandler,
    ApiAdapter: ApiAdapterTrait,
{
    backend: Backend,
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    membership: DaMembershipHandler<Membership>,
    addressbook: DaAddressbook,
    api_adapter: ApiAdapter,
    subnet_refresh_sender: Sender<()>,
    balancer_stats_stream: UnboundedReceiverStream<BalancerStats>,
    opinion_stream: UnboundedReceiverStream<OpinionEvent>,
    subnet_refresh_interval: Duration,
}

pub struct NetworkState<
    Backend,
    Membership,
    MembershipServiceAdapter,
    StorageAdapter,
    ApiAdapter,
    RuntimeServiceId,
> where
    Backend: NetworkBackend<RuntimeServiceId>,
{
    backend: Backend::State,
    phantom: PhantomData<(
        Membership,
        MembershipServiceAdapter,
        ApiAdapter,
        StorageAdapter,
    )>,
}

impl<
    Backend,
    Membership,
    MembershipServiceAdapter,
    StorageAdapter,
    ApiAdapter,
    SdpAdapter,
    RuntimeServiceId,
> ServiceData
    for NetworkService<
        Backend,
        Membership,
        MembershipServiceAdapter,
        StorageAdapter,
        ApiAdapter,
        SdpAdapter,
        RuntimeServiceId,
    >
where
    Backend: NetworkBackend<RuntimeServiceId> + 'static + Send,
    Membership: MembershipHandler,
    ApiAdapter: ApiAdapterTrait,
{
    type Settings = NetworkConfig<Backend, Membership, ApiAdapter::Settings, RuntimeServiceId>;
    type State = NetworkState<
        Backend,
        Membership,
        MembershipServiceAdapter,
        StorageAdapter,
        ApiAdapter,
        RuntimeServiceId,
    >;
    type StateOperator = NoOperator<Self::State>;
    type Message = DaNetworkMsg<Backend, DaSharesCommitments, RuntimeServiceId>;
}

#[async_trait]
impl<
    Backend,
    Membership,
    MembershipServiceAdapter,
    StorageAdapter,
    ApiAdapter,
    SdpAdapter,
    RuntimeServiceId,
> ServiceCore<RuntimeServiceId>
    for NetworkService<
        Backend,
        Membership,
        MembershipServiceAdapter,
        StorageAdapter,
        ApiAdapter,
        SdpAdapter,
        RuntimeServiceId,
    >
where
    Backend: NetworkBackend<
            RuntimeServiceId,
            Membership = DaMembershipHandler<Membership>,
            HistoricMembership = SharedMembershipHandler<Membership>,
            Addressbook = DaAddressbook,
        > + Send
        + Sync
        + 'static,
    Backend::State: Send + Sync,
    Membership:
        MembershipCreator<Id = PeerId, NetworkId = SubnetworkId> + Clone + Send + Sync + 'static,
    Membership::Id: Send + Sync,
    Membership::NetworkId: Send,
    MembershipServiceAdapter: MembershipAdapter<Id = Membership::Id> + Send + Sync + 'static,
    StorageAdapter: MembershipStorageAdapter<PeerId, SubnetworkId> + Send + Sync + 'static,
    ApiAdapter: ApiAdapterTrait<
            Share = DaShare,
            BlobId = BlobId,
            Commitments = DaSharesCommitments,
            Membership = DaMembershipHandler<Membership>,
            Addressbook = DaAddressbook,
        > + Send
        + Sync
        + 'static,
    ApiAdapter::Settings: Clone + Send + Sync,
    SdpAdapter: SdpAdapterTrait<RuntimeServiceId> + Send + Sync,
    <MembershipServiceAdapter::MembershipService as ServiceData>::Message: Send + Sync + 'static,
    RuntimeServiceId: AsServiceId<Self>
        + Clone
        + Display
        + Send
        + Sync
        + Debug
        + AsServiceId<MembershipServiceAdapter::MembershipService>
        + AsServiceId<StorageAdapter::StorageService>
        + 'static,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        _initial_state: Self::State,
    ) -> Result<Self, overwatch::DynError> {
        let settings = service_resources_handle
            .settings_handle
            .notifier()
            .get_updated_settings();

        let membership = DaMembershipHandler::new(settings.membership);
        let addressbook = DaAddressbook::default();

        let api_adapter = ApiAdapter::new(
            settings.api_adapter_settings.clone(),
            membership.clone(),
            addressbook.clone(),
        );

        // Sampling and balancer subnetwork peers need to be updated periodically.
        // They also need to be updated when the assignations change.
        let (subnet_refresh_sender, _) = broadcast::channel(1);
        let (balancer_stats_sender, balancer_stats_receiver) = mpsc::unbounded_channel();
        let balancer_stats_stream = UnboundedReceiverStream::new(balancer_stats_receiver);
        let (opinion_sender, opinion_receiver) = mpsc::unbounded_channel();
        let opinion_stream = UnboundedReceiverStream::new(opinion_receiver);

        Ok(Self {
            backend: <Backend as NetworkBackend<RuntimeServiceId>>::new(
                settings.backend,
                service_resources_handle.overwatch_handle.clone(),
                membership.clone(),
                addressbook.clone(),
                &subnet_refresh_sender,
                balancer_stats_sender,
                opinion_sender,
            ),
            service_resources_handle,
            membership,
            addressbook,
            api_adapter,
            subnet_refresh_sender,
            balancer_stats_stream,
            opinion_stream,
            subnet_refresh_interval: settings.subnet_refresh_interval,
        })
    }

    async fn run(mut self) -> Result<(), overwatch::DynError> {
        let Self {
            service_resources_handle:
                OpaqueServiceResourcesHandle::<Self, RuntimeServiceId> {
                    ref mut inbound_relay,
                    ref status_updater,
                    ref overwatch_handle,
                    ..
                },
            ref mut backend,
            ref membership,
            ref api_adapter,
            ref addressbook,
            ref subnet_refresh_sender,
            ref mut balancer_stats_stream,
            ref mut opinion_stream,
            ..
        } = self;

        let NetworkConfig {
            subnet_threshold,
            min_session_members,
            ..
        } = self
            .service_resources_handle
            .settings_handle
            .notifier()
            .get_updated_settings();

        let membership_service_relay = overwatch_handle
            .relay::<MembershipServiceAdapter::MembershipService>()
            .await?;

        let storage_service_relay = overwatch_handle
            .relay::<StorageAdapter::StorageService>()
            .await?;

        let storage_adapter = Arc::new(StorageAdapter::new(storage_service_relay));
        let membership_storage = MembershipStorage::new(
            Arc::clone(&storage_adapter),
            membership.clone(),
            addressbook.clone(),
            min_session_members,
        );

        let membership_service_adapter = MembershipServiceAdapter::new(membership_service_relay);

        let sdp_adapter = SdpAdapter::new(overwatch_handle).await?;

        let (local_peer_id, local_provider_id) = backend.local_peer_id();
        let mut opinion_aggregator =
            OpinionAggregator::new(storage_adapter, local_peer_id, local_provider_id);

        wait_until_services_are_ready!(
            &self.service_resources_handle.overwatch_handle,
            Some(Duration::from_secs(60)),
            <MembershipServiceAdapter as MembershipAdapter>::MembershipService
        )
        .await?;

        let mut membership_updates_stream =
            membership_service_adapter.subscribe().await.map_err(|e| {
                tracing::error!("Failed to subscribe to membership service: {e}");
                e
            })?;

        status_updater.notify_ready();

        let mut refresh_interval = tokio::time::interval(self.subnet_refresh_interval);

        tracing::info!(
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        loop {
            tokio::select! {
                Some(msg) = inbound_relay.recv() => {
                    Self::handle_network_service_message(msg, backend, &membership_storage, api_adapter, addressbook).await;
                }
                Some(update) = membership_updates_stream.next() => {
                    Self::handle_membership_update_event(
                        &sdp_adapter,
                        &mut opinion_aggregator,
                        update,
                        &membership_storage,
                        subnet_refresh_sender,
                        backend,
                    ).await;
                }
                Some(stats) = balancer_stats_stream.next() => {
                    let connected_subnetworks = stats.values()
                        .filter(|stats| stats.inbound > 0 || stats.outbound > 0)
                        .count();
                    if connected_subnetworks < subnet_threshold {
                        backend.update_connection_status(ConnectionStatus::InsufficientSubnetworkConnections);
                    } else {
                        backend.update_connection_status(ConnectionStatus::Ready);
                    }
                }
                Some(opinion_event) = opinion_stream.next() => {
                    opinion_aggregator.record_opinion(opinion_event);
                }
                _ = refresh_interval.tick() => {
                    let _ = subnet_refresh_sender.send(());
                }
            }
        }
    }
}

impl<
    Backend,
    Membership,
    MembershipServiceAdapter,
    StorageAdapter,
    ApiAdapter,
    SdpAdapter,
    RuntimeServiceId,
> Drop
    for NetworkService<
        Backend,
        Membership,
        MembershipServiceAdapter,
        StorageAdapter,
        ApiAdapter,
        SdpAdapter,
        RuntimeServiceId,
    >
where
    Backend: NetworkBackend<RuntimeServiceId> + Send + 'static,
    Membership: MembershipHandler,
    ApiAdapter: ApiAdapterTrait,
{
    fn drop(&mut self) {
        self.backend.shutdown();
    }
}

impl<
    Backend,
    Membership,
    MembershipServiceAdapter,
    StorageAdapter,
    ApiAdapter,
    SdpAdapter,
    RuntimeServiceId,
>
    NetworkService<
        Backend,
        Membership,
        MembershipServiceAdapter,
        StorageAdapter,
        ApiAdapter,
        SdpAdapter,
        RuntimeServiceId,
    >
where
    StorageAdapter: MembershipStorageAdapter<
            <Membership as MembershipHandler>::Id,
            <Membership as MembershipHandler>::NetworkId,
        > + Send
        + Sync,
    ApiAdapter:
        ApiAdapterTrait<BlobId = BlobId, Commitments = DaSharesCommitments> + Send + Sync + 'static,
    ApiAdapter::Settings: Clone + Send,
    SdpAdapter: SdpAdapterTrait<RuntimeServiceId> + Send + Sync,
    Backend: NetworkBackend<RuntimeServiceId, HistoricMembership = SharedMembershipHandler<Membership>>
        + Send
        + Sync
        + 'static,
    Backend::State: Send + Sync,
    Membership:
        MembershipCreator<Id = PeerId, NetworkId = SubnetworkId> + Clone + Send + Sync + 'static,
{
    async fn handle_network_service_message(
        msg: DaNetworkMsg<Backend, DaSharesCommitments, RuntimeServiceId>,
        backend: &mut Backend,
        membership_storage: &MembershipStorage<Arc<StorageAdapter>, Membership, DaAddressbook>,
        api_adapter: &ApiAdapter,
        addressbook: &DaAddressbook,
    ) {
        match msg {
            DaNetworkMsg::Process(msg) => {
                // split sending in two steps to help the compiler understand we do not
                // need to hold an instance of &I (which is not Send) across an await point
                let send = backend.process(msg);
                send.await;
            }
            DaNetworkMsg::Subscribe { kind, sender } => sender
                .send(backend.subscribe(kind).await)
                .unwrap_or_else(|_| {
                    tracing::warn!(
                        "client hung up before a subscription handle could be established"
                    );
                }),
            DaNetworkMsg::GetMembership { session_id, sender } => {
                // todo: handle errors properly when the usage of this function is known
                // now we are just logging and returning an empty assignations
                if let Some(membership) = membership_storage
                    .get_historic_membership(session_id)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::error!(
                            "Failed to get historic membership for session {session_id}: {e}"
                        );
                        None
                    })
                {
                    let assignations = membership.subnetworks();
                    let addressbook = assignations
                        .values()
                        .flatten()
                        .filter_map(|id| {
                            addressbook
                                .get_address(id)
                                .map(|address| (*id, address))
                                .or_else(|| {
                                    tracing::error!("No address found for peer {id:?}");
                                    None
                                })
                        })
                        .collect::<AddressBookSnapshot<_>>();
                    sender.send(MembershipResponse { assignations, addressbook }).unwrap_or_else(|_| {
                                tracing::warn!(
                                    "client hung up before a subnetwork assignations handle could be established"
                                );
                            });
                } else {
                    tracing::warn!("No membership found for session {session_id}");
                    sender.send(MembershipResponse{
                                assignations: SubnetworkAssignations::default(),
                                addressbook: AddressBookSnapshot::default(),
                            }).unwrap_or_else(|_| {
                                tracing::warn!(
                                    "client hung up before a subnetwork assignations handle could be established"
                                );
                            });
                }
            }
            DaNetworkMsg::GetCommitments { blob_id, sender } => {
                if let Err(e) = api_adapter.request_commitments(blob_id, sender).await {
                    tracing::error!("Failed to request commitments: {e}");
                }
            }
            DaNetworkMsg::RequestHistoricSampling { blob_ids, block_id } => {
                Self::handle_historic_sample_request(
                    backend,
                    membership_storage,
                    block_id,
                    blob_ids,
                )
                .await;
            }
            DaNetworkMsg::RequestHistoricCommitments {
                block_id,
                blob_id,
                session,
            } => {
                Self::handle_historic_commitments_request(
                    backend,
                    membership_storage,
                    block_id,
                    blob_id,
                    session,
                )
                .await;
            }
        }
    }

    async fn handle_membership_update_event(
        sdp_adapter: &SdpAdapter,
        opinion_aggregator: &mut OpinionAggregator<Arc<StorageAdapter>>,
        update: SubnetworkPeers<Membership::Id>,
        membership_storage: &MembershipStorage<Arc<StorageAdapter>, Membership, DaAddressbook>,
        subnet_refresh_sender: &Sender<()>,
        backend: &mut Backend,
    ) {
        Self::handle_opinion_session_change(sdp_adapter, opinion_aggregator, &update).await;

        match Self::handle_membership_update(
            update.session_id,
            update.peers.clone(),
            membership_storage,
            update.provider_mappings.clone(),
        )
        .await
        {
            Ok(membership_status) => {
                let _ = subnet_refresh_sender.send(());
                backend.update_session_status(membership_status);
            }
            Err(e) => {
                tracing::error!(
                    "Error handling membership update for session {}: {e}",
                    update.session_id
                );
            }
        }

        if let Some(cutoff) = update.session_id.checked_sub(ASSIGNATIONS_PRUNE_WINDOW)
            && let Err(e) = membership_storage.prune(cutoff).await
        {
            tracing::error!(
                "Error membership prune on session {}: {e}",
                update.session_id
            );
        }
    }

    async fn handle_membership_update(
        session_id: SessionNumber,
        update: HashMap<Membership::Id, Multiaddr>,
        storage: &MembershipStorage<Arc<StorageAdapter>, Membership, DaAddressbook>,
        provider_mappings: HashMap<Membership::Id, ProviderId>,
    ) -> Result<SessionStatus, DynError> {
        storage.update(session_id, update, provider_mappings).await
    }

    async fn handle_historic_sample_request(
        backend: &Backend,
        membership_storage: &MembershipStorage<Arc<StorageAdapter>, Membership, AddressBook>,
        block_id: HeaderId,
        blob_ids: HashMap<BlobId, SessionNumber>,
    ) {
        let session_ids: HashSet<SessionNumber> = blob_ids.values().copied().collect();

        let mut session_to_membership = HashMap::new();

        for session_id in session_ids {
            let membership = membership_storage
                .get_historic_membership(session_id)
                .await
                .unwrap_or_else(|e| {
                    tracing::error!(
                        "Failed to get historic membership for session {session_id}: {e}"
                    );
                    None
                });

            if let Some(membership) = membership {
                let membership = SharedMembershipHandler::new(membership);
                session_to_membership.insert(session_id, membership);
            } else {
                tracing::error!("No membership found for session {session_id}");
            }
        }

        // Group blobs by membership
        let mut blobs_by_membership: HashMap<SharedMembershipHandler<Membership>, HashSet<BlobId>> =
            HashMap::new();

        for (blob_id, session_id) in blob_ids {
            if let Some(membership) = session_to_membership.get(&session_id) {
                blobs_by_membership
                    .entry(membership.clone())
                    .or_default()
                    .insert(blob_id);
            }
        }

        backend
            .start_historic_sampling(block_id, blobs_by_membership)
            .await;
    }

    async fn handle_historic_commitments_request(
        backend: &Backend,
        membership_storage: &MembershipStorage<Arc<StorageAdapter>, Membership, AddressBook>,
        block_id: HeaderId,
        blob_id: BlobId,
        session: SessionNumber,
    ) {
        let membership = membership_storage
            .get_historic_membership(session)
            .await
            .unwrap_or_else(|e| {
                tracing::error!("Failed to get historic membership for session {session}: {e}");
                None
            });

        if let Some(membership) = membership {
            let membership = SharedMembershipHandler::new(membership);

            backend
                .start_historic_commitments(block_id, blob_id, membership)
                .await;
        } else {
            tracing::error!(
                "No membership found for session {session}, cannot request historic commitments"
            );
        }
    }

    async fn handle_opinion_session_change(
        sdp_adapter: &SdpAdapter,
        opinion_aggregator: &mut OpinionAggregator<Arc<StorageAdapter>>,
        new_providers: &SubnetworkPeers<Membership::Id>,
    ) {
        let opinions = match opinion_aggregator
            .handle_session_change(new_providers)
            .await
        {
            Ok(opinions) => opinions,
            Err(OpinionError::InsufficientData) => {
                tracing::debug!(
                    "Insufficient data to generate opinions (first session), skip forming session"
                );
                return;
            }
            Err(OpinionError::Error(e)) => {
                tracing::error!("Failed to generate opinions: {e}");
                return;
            }
        };

        if let Err(err) = sdp_adapter.post_activity(opinions).await {
            tracing::error!("Could not post activity opinions: {err}");
        }
    }
}

impl<Backend: NetworkBackend<RuntimeServiceId>, Membership, ApiAdapterSettings, RuntimeServiceId>
    Clone for NetworkConfig<Backend, Membership, ApiAdapterSettings, RuntimeServiceId>
where
    Membership: Clone,
    ApiAdapterSettings: Clone,
{
    fn clone(&self) -> Self {
        Self {
            backend: self.backend.clone(),
            membership: self.membership.clone(),
            api_adapter_settings: self.api_adapter_settings.clone(),
            subnet_refresh_interval: self.subnet_refresh_interval,
            subnet_threshold: self.subnet_threshold,
            min_session_members: self.min_session_members,
        }
    }
}

impl<
    Backend: NetworkBackend<RuntimeServiceId>,
    Membership,
    MembershipServiceAdapter,
    StorageAdapter,
    ApiAdapter,
    RuntimeServiceId,
> Clone
    for NetworkState<
        Backend,
        Membership,
        MembershipServiceAdapter,
        StorageAdapter,
        ApiAdapter,
        RuntimeServiceId,
    >
{
    fn clone(&self) -> Self {
        Self {
            backend: self.backend.clone(),
            phantom: PhantomData,
        }
    }
}

impl<
    Backend: NetworkBackend<RuntimeServiceId>,
    Membership,
    MembershipServiceAdapter,
    StorageAdapter,
    ApiAdapter,
    RuntimeServiceId,
> ServiceState
    for NetworkState<
        Backend,
        Membership,
        MembershipServiceAdapter,
        StorageAdapter,
        ApiAdapter,
        RuntimeServiceId,
    >
where
    Membership: Clone,
    ApiAdapter: ApiAdapterTrait,
{
    type Settings = NetworkConfig<Backend, Membership, ApiAdapter::Settings, RuntimeServiceId>;
    type Error = <Backend::State as ServiceState>::Error;

    fn from_settings(settings: &Self::Settings) -> Result<Self, Self::Error> {
        Backend::State::from_settings(&settings.backend).map(|backend| Self {
            backend,
            phantom: PhantomData,
        })
    }
}
