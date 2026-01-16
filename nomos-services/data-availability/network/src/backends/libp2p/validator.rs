use std::{
    collections::{HashMap, HashSet},
    fmt::Debug,
    marker::PhantomData,
    pin::Pin,
};

use futures::{
    Stream, StreamExt as _,
    future::{AbortHandle, Abortable},
};
use libp2p::PeerId;
use nomos_core::{
    da::BlobId,
    header::HeaderId,
    sdp::{ProviderId, SessionNumber},
};
use nomos_da_network_core::{
    SubnetworkId,
    maintenance::{balancer::ConnectionBalancerCommand, monitor::ConnectionMonitorCommand},
    protocols::sampling::opinions::OpinionEvent,
    swarm::{
        BalancerStats, MonitorStats,
        validator::{CommitmentsArgs, SampleArgs, SwarmSettings, ValidatorSwarm},
    },
};
use nomos_libp2p::ed25519;
use nomos_tracing::info_with_id;
use overwatch::{overwatch::handle::OverwatchHandle, services::state::NoState};
use serde::Serialize;
use subnetworks_assignations::MembershipHandler;
use tokio::sync::{broadcast, mpsc::UnboundedSender, oneshot};
use tokio_stream::wrappers::BroadcastStream;
use tracing::instrument;

use crate::{
    DaAddressbook, SessionStatus,
    backends::{
        ConnectionStatus, NetworkBackend,
        libp2p::common::{
            BROADCAST_CHANNEL_SIZE, CommitmentsEvent, DaNetworkBackendSettings,
            HistoricSamplingEvent, SamplingEvent, VerificationEvent, handle_balancer_command,
            handle_historic_commitments_request, handle_historic_sample_request,
            handle_monitor_command, handle_sample_request, handle_validator_events_stream,
        },
    },
    membership::handler::{DaMembershipHandler, SharedMembershipHandler},
};

/// Message that the backend replies to
#[derive(Debug)]
pub enum DaNetworkMessage<BalancerStats, MonitorStats>
where
    BalancerStats: Debug + Serialize,
{
    /// Kickstart a network sampling
    RequestSample {
        blob_id: BlobId,
        session: SessionNumber,
    },
    RequestCommitments {
        blob_id: BlobId,
        session: SessionNumber,
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
    HistoricSampling,
}

/// DA network incoming events
#[derive(Debug)]
pub enum DaNetworkEvent {
    Sampling(SamplingEvent),
    Commitments(CommitmentsEvent),
    Verifying(VerificationEvent),
    HistoricSampling(HistoricSamplingEvent),
}

/// DA network backend for validators
/// Internally uses a libp2p swarm composed of the [`ValidatorBehaviour`]
/// It forwards network messages to the corresponding subscription
/// channels/streams
pub struct DaNetworkValidatorBackend<Membership> {
    connection_status: ConnectionStatus,
    session_status: SessionStatus,
    task_abort_handle: AbortHandle,
    replies_task_abort_handle: AbortHandle,
    shares_request_channel: UnboundedSender<(BlobId, SessionNumber)>,
    commitments_request_channel: UnboundedSender<(BlobId, SessionNumber)>,
    historic_sample_request_channel:
        UnboundedSender<SampleArgs<SharedMembershipHandler<Membership>>>,
    historic_commitments_request_channel:
        UnboundedSender<CommitmentsArgs<SharedMembershipHandler<Membership>>>,
    balancer_command_sender: UnboundedSender<ConnectionBalancerCommand<BalancerStats>>,
    monitor_command_sender: UnboundedSender<ConnectionMonitorCommand<MonitorStats>>,
    sampling_broadcast_receiver: broadcast::Receiver<SamplingEvent>,
    commitments_broadcast_receiver: broadcast::Receiver<CommitmentsEvent>,
    verifying_broadcast_receiver: broadcast::Receiver<VerificationEvent>,
    historic_sampling_broadcast_receiver: broadcast::Receiver<HistoricSamplingEvent>,
    local_peer_id: PeerId,
    local_provider_id: ProviderId,
    _membership: PhantomData<Membership>,
}

#[async_trait::async_trait]
impl<Membership, RuntimeServiceId> NetworkBackend<RuntimeServiceId>
    for DaNetworkValidatorBackend<Membership>
where
    Membership: MembershipHandler<NetworkId = SubnetworkId, Id = PeerId>
        + Clone
        + Debug
        + Send
        + Sync
        + 'static,
    BalancerStats: Debug + Serialize + Send + Sync + 'static,
{
    type Settings = DaNetworkBackendSettings;
    type State = NoState<Self::Settings>;
    type Message = DaNetworkMessage<BalancerStats, MonitorStats>;
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
        let ed_keypair = ed25519::Keypair::from(config.node_key.clone());
        let local_provider_id = ProviderId::try_from(ed_keypair.public().to_bytes())
            .expect("Valid Ed25519 public key from keypair");
        let keypair = libp2p::identity::Keypair::from(ed_keypair);

        let (mut validator_swarm, validator_events_stream) = ValidatorSwarm::new(
            keypair,
            membership,
            addressbook,
            SwarmSettings {
                policy_settings: config.policy_settings,
                monitor_settings: config.monitor_settings,
                balancer_interval: config.balancer_interval,
                redial_cooldown: config.redial_cooldown,
                replication_settings: config.replication_settings,
                subnets_settings: config.subnets_settings,
            },
            subnet_refresh_sender,
            balancer_stats_sender,
        );

        let local_peer_id = *validator_swarm.local_peer_id();

        let address = config.listening_address;
        // put swarm to listen at the specified configuration address
        validator_swarm
            .protocol_swarm_mut()
            .listen_on(address.clone())
            .unwrap_or_else(|e| {
                panic!("Error listening on DA network with address {address}: {e}")
            });

        let shares_request_channel = validator_swarm.shares_request_channel();
        let commitments_request_channel = validator_swarm.commitments_request_channel();
        let historic_sample_request_channel = validator_swarm.historic_sample_request_channel();
        let historic_commitments_request_channel =
            validator_swarm.historic_commitments_request_channel();
        let balancer_command_sender = validator_swarm.balancer_command_channel();
        let monitor_command_sender = validator_swarm.monitor_command_channel();

        let (task_abort_handle, abort_registration) = AbortHandle::new_pair();
        overwatch_handle
            .runtime()
            .spawn(Abortable::new(validator_swarm.run(), abort_registration));

        let (sampling_broadcast_sender, sampling_broadcast_receiver) =
            broadcast::channel(BROADCAST_CHANNEL_SIZE);
        let (commitments_broadcast_sender, commitments_broadcast_receiver) =
            broadcast::channel(BROADCAST_CHANNEL_SIZE);
        let (verifying_broadcast_sender, verifying_broadcast_receiver) =
            broadcast::channel(BROADCAST_CHANNEL_SIZE);
        let (historic_sampling_broadcast_sender, historic_sampling_broadcast_receiver) =
            broadcast::channel(BROADCAST_CHANNEL_SIZE);

        let (replies_task_abort_handle, replies_task_abort_registration) = AbortHandle::new_pair();
        overwatch_handle.runtime().spawn(Abortable::new(
            handle_validator_events_stream(
                validator_events_stream,
                sampling_broadcast_sender,
                commitments_broadcast_sender,
                verifying_broadcast_sender,
                historic_sampling_broadcast_sender,
                opinion_sender,
            ),
            replies_task_abort_registration,
        ));

        Self {
            local_peer_id,
            local_provider_id,
            connection_status: ConnectionStatus::InsufficientSubnetworkConnections,
            session_status: SessionStatus::InsufficientMembers,
            task_abort_handle,
            replies_task_abort_handle,
            shares_request_channel,
            commitments_request_channel,
            historic_sample_request_channel,
            historic_commitments_request_channel,
            balancer_command_sender,
            monitor_command_sender,
            sampling_broadcast_receiver,
            commitments_broadcast_receiver,
            verifying_broadcast_receiver,
            historic_sampling_broadcast_receiver,
            _membership: PhantomData,
        }
    }

    fn shutdown(&mut self) {
        let Self {
            task_abort_handle,
            replies_task_abort_handle,
            ..
        } = self;
        task_abort_handle.abort();
        replies_task_abort_handle.abort();
    }

    #[instrument(skip_all)]
    async fn process(&self, msg: Self::Message) {
        match msg {
            DaNetworkMessage::RequestSample { blob_id, session } => {
                info_with_id!(&blob_id, "RequestSample");
                handle_sample_request(&self.shares_request_channel, blob_id, session).await;
            }
            DaNetworkMessage::RequestCommitments { blob_id, session } => {
                info_with_id!(&blob_id, "RequestSample");
                handle_sample_request(&self.commitments_request_channel, blob_id, session).await;
            }
            DaNetworkMessage::MonitorRequest(command) => {
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
            DaNetworkMessage::BalancerStats(response_sender) => {
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
                    .filter_map(async |event| event.ok())
                    .map(Self::NetworkEvent::Sampling),
            ),
            DaNetworkEventKind::Commitments => Box::pin(
                BroadcastStream::new(self.commitments_broadcast_receiver.resubscribe())
                    .filter_map(async |event| event.ok())
                    .map(Self::NetworkEvent::Commitments),
            ),
            DaNetworkEventKind::Verifying => Box::pin(
                BroadcastStream::new(self.verifying_broadcast_receiver.resubscribe())
                    .filter_map(async |event| event.ok())
                    .map(Self::NetworkEvent::Verifying),
            ),
            DaNetworkEventKind::HistoricSampling => Box::pin(
                BroadcastStream::new(self.historic_sampling_broadcast_receiver.resubscribe())
                    .filter_map(async |event| event.ok())
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
