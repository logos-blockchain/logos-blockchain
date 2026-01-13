use std::{
    collections::{HashMap, HashSet},
    fmt::Debug,
    marker::PhantomData,
    pin::Pin,
};

use futures::{
    Stream, StreamExt as _,
    stream::{AbortHandle, Abortable},
};
use kzgrs_backend::common::share::DaShare;
use libp2p::PeerId;
use log::error;
use nomos_core::{
    da::BlobId,
    header::HeaderId,
    mantle::SignedMantleTx,
    sdp::{ProviderId, SessionNumber},
};
use nomos_da_network_core::{
    SubnetworkId,
    maintenance::{balancer::ConnectionBalancerCommand, monitor::ConnectionMonitorCommand},
    protocols::{
        dispersal::executor::behaviour::DispersalExecutorEvent, sampling::opinions::OpinionEvent,
    },
    swarm::{
        BalancerStats, MonitorStats,
        executor::ExecutorSwarm,
        validator::{CommitmentsArgs, SampleArgs, SwarmSettings},
    },
};
use nomos_libp2p::ed25519;
use nomos_tracing::info_with_id;
use overwatch::{overwatch::handle::OverwatchHandle, services::state::NoState};
use serde::{Deserialize, Serialize};
use subnetworks_assignations::MembershipHandler;
use tokio::sync::{broadcast, mpsc::UnboundedSender, oneshot};
use tokio_stream::wrappers::{
    BroadcastStream, UnboundedReceiverStream, errors::BroadcastStreamRecvError,
};
use tracing::instrument;

use super::common::{CommitmentsEvent, VerificationEvent};
use crate::{
    DaAddressbook, SessionStatus,
    backends::{
        ConnectionStatus, NetworkBackend, ProcessingError,
        libp2p::common::{
            BROADCAST_CHANNEL_SIZE, DaNetworkBackendSettings, HistoricSamplingEvent, SamplingEvent,
            handle_balancer_command, handle_historic_commitments_request,
            handle_historic_sample_request, handle_monitor_command, handle_sample_request,
            handle_validator_events_stream,
        },
    },
    membership::handler::{DaMembershipHandler, SharedMembershipHandler},
};

/// Message that the backend replies to
#[derive(Debug)]
pub enum ExecutorDaNetworkMessage<BalancerStats, MonitorStats> {
    /// Kickstart a network sapling
    RequestSample {
        blob_id: BlobId,
        session: SessionNumber,
    },
    RequestCommitments {
        blob_id: BlobId,
        session: SessionNumber,
    },
    RequestShareDispersal {
        subnetwork_id: SubnetworkId,
        da_share: Box<DaShare>,
        sender: oneshot::Sender<Result<(), ProcessingError>>,
    },
    RequestTxDispersal {
        subnetwork_id: SubnetworkId,
        tx: Box<SignedMantleTx>,
        sender: oneshot::Sender<Result<(), ProcessingError>>,
    },
    MonitorRequest(ConnectionMonitorCommand<MonitorStats>),
    BalancerStats(oneshot::Sender<BalancerStats>),
}

/// Events types to subscribe to
/// * Sampling: Incoming sampling events [success/fail]
/// * Incoming blobs to be verified
#[derive(Debug)]
pub enum DaNetworkEventKind {
    Sampling,
    Commitments,
    Verifying,
    Dispersal,
    HistoricSampling,
}

/// DA network incoming events
#[derive(Debug)]
pub enum DaNetworkEvent {
    Sampling(SamplingEvent),
    Commitments(CommitmentsEvent),
    Verifying(VerificationEvent),
    Dispersal(DispersalExecutorEvent),
    HistoricSampling(HistoricSamplingEvent),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DaNetworkExecutorBackendSettings {
    pub validator_settings: DaNetworkBackendSettings,
    pub num_subnets: u16,
}

/// DA network backend for validators
/// Internally uses a libp2p swarm composed of the [`ExecutorBehaviour`]
/// It forwards network messages to the corresponding subscription
/// channels/streams
pub struct DaNetworkExecutorBackend<Membership>
where
    Membership: MembershipHandler,
{
    task_abort_handle: AbortHandle,
    verifier_replies_task_abort_handle: AbortHandle,
    executor_replies_task_abort_handle: AbortHandle,
    shares_request_channel: UnboundedSender<(BlobId, SessionNumber)>,
    historic_sample_request_channel:
        UnboundedSender<SampleArgs<SharedMembershipHandler<Membership>>>,
    historic_commitments_request_channel:
        UnboundedSender<CommitmentsArgs<SharedMembershipHandler<Membership>>>,
    commitments_request_channel: UnboundedSender<(BlobId, SessionNumber)>,
    sampling_broadcast_receiver: broadcast::Receiver<SamplingEvent>,
    commitments_broadcast_receiver: broadcast::Receiver<CommitmentsEvent>,
    verifying_broadcast_receiver: broadcast::Receiver<VerificationEvent>,
    dispersal_broadcast_receiver: broadcast::Receiver<DispersalExecutorEvent>,
    historic_sampling_broadcast_receiver: broadcast::Receiver<HistoricSamplingEvent>,
    dispersal_shares_sender: UnboundedSender<(Membership::NetworkId, DaShare)>,
    dispersal_tx_sender: UnboundedSender<(Membership::NetworkId, SignedMantleTx)>,
    balancer_command_sender: UnboundedSender<ConnectionBalancerCommand<BalancerStats>>,
    monitor_command_sender: UnboundedSender<ConnectionMonitorCommand<MonitorStats>>,
    connection_status: ConnectionStatus,
    session_status: SessionStatus,
    local_peer_id: PeerId,
    local_provider_id: ProviderId,
    _membership: PhantomData<Membership>,
}

#[async_trait::async_trait]
impl<Membership, RuntimeServiceId> NetworkBackend<RuntimeServiceId>
    for DaNetworkExecutorBackend<Membership>
where
    Membership: MembershipHandler<NetworkId = SubnetworkId, Id = PeerId>
        + Clone
        + Debug
        + Send
        + Sync
        + 'static,
    BalancerStats: Debug + Serialize + Send + Sync + 'static,
{
    type Settings = DaNetworkExecutorBackendSettings;
    type State = NoState<Self::Settings>;
    type Message = ExecutorDaNetworkMessage<BalancerStats, MonitorStats>;
    type EventKind = DaNetworkEventKind;
    type NetworkEvent = DaNetworkEvent;
    type HistoricMembership = SharedMembershipHandler<Membership>;
    type Membership = DaMembershipHandler<Membership>;
    type Addressbook = DaAddressbook;

    fn new(
        config: Self::Settings,
        overwatch_handle: OverwatchHandle<RuntimeServiceId>,
        membership: Self::Membership,
        addressbook: Self::Addressbook,
        subnet_refresh_sender: &broadcast::Sender<()>,
        balancer_stats_sender: UnboundedSender<BalancerStats>,
        opinion_sender: UnboundedSender<OpinionEvent>,
    ) -> Self {
        let ed_keypair = ed25519::Keypair::from(config.validator_settings.node_key.clone());
        let local_provider_id = ProviderId::try_from(ed_keypair.public().to_bytes())
            .expect("Valid Ed25519 public key from keypair");
        let keypair = libp2p::identity::Keypair::from(ed_keypair);

        let (mut executor_swarm, executor_events_stream) = ExecutorSwarm::new(
            keypair,
            membership,
            addressbook,
            SwarmSettings {
                policy_settings: config.validator_settings.policy_settings,
                monitor_settings: config.validator_settings.monitor_settings,
                balancer_interval: config.validator_settings.balancer_interval,
                redial_cooldown: config.validator_settings.redial_cooldown,
                replication_settings: config.validator_settings.replication_settings,
                subnets_settings: config.validator_settings.subnets_settings,
            },
            subnet_refresh_sender,
            balancer_stats_sender,
        );

        let local_peer_id = *executor_swarm.local_peer_id();
        let address = config.validator_settings.listening_address;
        // put swarm to listen at the specified configuration address
        executor_swarm
            .protocol_swarm_mut()
            .listen_on(address.clone())
            .unwrap_or_else(|e| {
                panic!("Error listening on DA network with address {address}: {e}")
            });

        let shares_request_channel = executor_swarm.shares_request_channel();
        let historic_sample_request_channel = executor_swarm.historic_sample_request_channel();
        let historic_commitments_request_channel =
            executor_swarm.historic_commitments_request_channel();
        let commitments_request_channel = executor_swarm.commitments_request_channel();
        let dispersal_shares_sender = executor_swarm.dispersal_shares_channel();
        let dispersal_tx_sender = executor_swarm.dispersal_tx_channel();
        let balancer_command_sender = executor_swarm.balancer_command_channel();
        let monitor_command_sender = executor_swarm.monitor_command_channel();

        let (task_abort_handle, abort_registration) = AbortHandle::new_pair();
        overwatch_handle
            .runtime()
            .spawn(Abortable::new(executor_swarm.run(), abort_registration));

        let (sampling_broadcast_sender, sampling_broadcast_receiver) =
            broadcast::channel(BROADCAST_CHANNEL_SIZE);
        let (commitments_broadcast_sender, commitments_broadcast_receiver) =
            broadcast::channel(BROADCAST_CHANNEL_SIZE);
        let (verifying_broadcast_sender, verifying_broadcast_receiver) =
            broadcast::channel(BROADCAST_CHANNEL_SIZE);
        let (dispersal_broadcast_sender, dispersal_broadcast_receiver) =
            broadcast::channel(BROADCAST_CHANNEL_SIZE);
        let (historic_sampling_broadcast_sender, historic_sampling_broadcast_receiver) =
            broadcast::channel(BROADCAST_CHANNEL_SIZE);

        let (verifier_replies_task_abort_handle, verifier_replies_task_abort_registration) =
            AbortHandle::new_pair();
        overwatch_handle.runtime().spawn(Abortable::new(
            handle_validator_events_stream(
                executor_events_stream.validator_events_stream,
                sampling_broadcast_sender,
                commitments_broadcast_sender,
                verifying_broadcast_sender,
                historic_sampling_broadcast_sender,
                opinion_sender,
            ),
            verifier_replies_task_abort_registration,
        ));

        let (executor_replies_task_abort_handle, executor_replies_task_abort_registration) =
            AbortHandle::new_pair();
        overwatch_handle.runtime().spawn(Abortable::new(
            handle_executor_dispersal_events_stream(
                executor_events_stream.dispersal_events_receiver,
                dispersal_broadcast_sender,
            ),
            executor_replies_task_abort_registration,
        ));

        Self {
            local_peer_id,
            local_provider_id,
            connection_status: ConnectionStatus::InsufficientSubnetworkConnections,
            session_status: SessionStatus::InsufficientMembers,
            task_abort_handle,
            verifier_replies_task_abort_handle,
            executor_replies_task_abort_handle,
            shares_request_channel,
            historic_sample_request_channel,
            historic_commitments_request_channel,
            commitments_request_channel,
            sampling_broadcast_receiver,
            commitments_broadcast_receiver,
            verifying_broadcast_receiver,
            dispersal_broadcast_receiver,
            historic_sampling_broadcast_receiver,
            dispersal_shares_sender,
            dispersal_tx_sender,
            balancer_command_sender,
            monitor_command_sender,
            _membership: PhantomData,
        }
    }

    fn shutdown(&mut self) {
        let Self {
            task_abort_handle,
            verifier_replies_task_abort_handle,
            executor_replies_task_abort_handle,
            ..
        } = self;
        task_abort_handle.abort();
        verifier_replies_task_abort_handle.abort();
        executor_replies_task_abort_handle.abort();
    }

    #[instrument(skip_all)]
    async fn process(&self, msg: Self::Message) {
        match msg {
            ExecutorDaNetworkMessage::RequestSample { blob_id, session } => {
                info_with_id!(&blob_id, "RequestSample");
                handle_sample_request(&self.shares_request_channel, blob_id, session).await;
            }
            ExecutorDaNetworkMessage::RequestCommitments { blob_id, session } => {
                info_with_id!(&blob_id, "RequestSample");
                handle_sample_request(&self.commitments_request_channel, blob_id, session).await;
            }
            ExecutorDaNetworkMessage::RequestShareDispersal {
                subnetwork_id,
                da_share,
                sender,
            } => {
                if !matches!(self.session_status, SessionStatus::SufficientMembers) {
                    if let Err(e) = sender.send(Err(ProcessingError::InsufficientSessionMembers)) {
                        tracing::error!("Could not send error response for dispersal: {e:?}");
                    }
                    return;
                }
                if !matches!(self.connection_status, ConnectionStatus::Ready) {
                    if let Err(e) =
                        sender.send(Err(ProcessingError::InsufficientSubnetworkConnections))
                    {
                        tracing::error!("Could not send error response for dispersal: {e:?}");
                    }
                    return;
                }
                info_with_id!(&da_share.blob_id(), "RequestShareDispersal");
                if let Err(e) = self
                    .dispersal_shares_sender
                    .send((subnetwork_id, *da_share))
                {
                    error!("Could not send internal blob to underlying dispersal behaviour: {e}");
                }
                let _ = sender.send(Ok(()));
            }
            ExecutorDaNetworkMessage::RequestTxDispersal {
                subnetwork_id,
                tx,
                sender,
            } => {
                if !matches!(self.session_status, SessionStatus::SufficientMembers) {
                    if let Err(e) = sender.send(Err(ProcessingError::InsufficientSessionMembers)) {
                        tracing::error!("Could not send error response for dispersal: {e:?}");
                    }
                    return;
                }
                if !matches!(self.connection_status, ConnectionStatus::Ready) {
                    let _ = sender.send(Err(ProcessingError::InsufficientSubnetworkConnections));
                    return;
                }
                if let Err(e) = self.dispersal_tx_sender.send((subnetwork_id, *tx)) {
                    error!("Could not send internal tx to underlying dispersal behaviour: {e}");
                }
                let _ = sender.send(Ok(()));
            }
            ExecutorDaNetworkMessage::MonitorRequest(command) => {
                match command.peer_id() {
                    Some(peer_id) => {
                        tracing::info!(%peer_id, "{}", command.discriminant());
                    }
                    None => {
                        tracing::info!("{}", command.discriminant());
                    }
                }
                handle_monitor_command(&self.monitor_command_sender, command).await;
            }
            ExecutorDaNetworkMessage::BalancerStats(response_sender) => {
                tracing::info!("BalancerStats");
                handle_balancer_command(&self.balancer_command_sender, response_sender).await;
            }
        }
    }

    fn update_connection_status(&mut self, status: ConnectionStatus) {
        self.connection_status = status;
    }

    fn update_session_status(&mut self, status: SessionStatus) {
        self.session_status = status;
    }

    async fn subscribe(
        &mut self,
        event: Self::EventKind,
    ) -> Pin<Box<dyn Stream<Item = Self::NetworkEvent> + Send>> {
        match event {
            DaNetworkEventKind::Sampling => Box::pin(
                BroadcastStream::new(self.sampling_broadcast_receiver.resubscribe())
                    .filter_map(handle_stream_event)
                    .map(Self::NetworkEvent::Sampling),
            ),
            DaNetworkEventKind::Commitments => Box::pin(
                BroadcastStream::new(self.commitments_broadcast_receiver.resubscribe())
                    .filter_map(handle_stream_event)
                    .map(Self::NetworkEvent::Commitments),
            ),
            DaNetworkEventKind::Verifying => Box::pin(
                BroadcastStream::new(self.verifying_broadcast_receiver.resubscribe())
                    .filter_map(handle_stream_event)
                    .map(Self::NetworkEvent::Verifying),
            ),
            DaNetworkEventKind::Dispersal => Box::pin(
                BroadcastStream::new(self.dispersal_broadcast_receiver.resubscribe())
                    .filter_map(handle_stream_event)
                    .map(Self::NetworkEvent::Dispersal),
            ),
            DaNetworkEventKind::HistoricSampling => Box::pin(
                BroadcastStream::new(self.historic_sampling_broadcast_receiver.resubscribe())
                    .filter_map(handle_stream_event)
                    .map(Self::NetworkEvent::HistoricSampling),
            ),
        }
    }

    async fn start_historic_sampling(
        &self,
        block_id: HeaderId,
        blob_ids: HashMap<Self::HistoricMembership, HashSet<BlobId>>,
    ) {
        handle_historic_sample_request(&self.historic_sample_request_channel, block_id, blob_ids)
            .await;
    }

    async fn start_historic_commitments(
        &self,
        block_id: HeaderId,
        blob_id: BlobId,
        session: Self::HistoricMembership,
    ) {
        handle_historic_commitments_request(
            &self.historic_commitments_request_channel,
            block_id,
            blob_id,
            session,
        )
        .await;
    }

    fn local_peer_id(&self) -> (PeerId, ProviderId) {
        (self.local_peer_id, self.local_provider_id)
    }
}

async fn handle_executor_dispersal_events_stream(
    mut dispersal_events_receiver: UnboundedReceiverStream<DispersalExecutorEvent>,
    dispersal_broadcast_sender: broadcast::Sender<DispersalExecutorEvent>,
) {
    while let Some(event) = dispersal_events_receiver.next().await {
        if let Err(e) = dispersal_broadcast_sender.send(event) {
            error!("Error forwarding internal dispersal executor event: {e}");
        }
    }
}

async fn handle_stream_event<Event>(
    event: Result<Event, BroadcastStreamRecvError>,
) -> Option<Event> {
    event
        .inspect_err(|err| tracing::error!("Stream event broadcast error: {err:?}"))
        .ok()
}
