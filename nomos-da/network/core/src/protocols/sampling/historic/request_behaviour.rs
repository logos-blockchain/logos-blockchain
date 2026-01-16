use std::{
    collections::{HashMap, HashSet, VecDeque},
    future::ready,
    io::ErrorKind,
    task::{Context, Poll},
};

use futures::{
    AsyncWriteExt as _, FutureExt as _, StreamExt as _,
    future::BoxFuture,
    stream::{BoxStream, FuturesUnordered},
};
use kzgrs_backend::common::share::{DaLightShare, DaSharesCommitments};
use libp2p::{
    Multiaddr, PeerId,
    core::{Endpoint, transport::PortUse},
    swarm::{
        ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler, THandlerInEvent,
        THandlerOutEvent, ToSwarm, dial_opts::DialOpts,
    },
};
use libp2p_stream::{Control, OpenStreamError};
use nomos_core::{da::BlobId, header::HeaderId, sdp::SessionNumber};
use nomos_da_messages::sampling::{self, SampleResponse};
use rand::{rngs::ThreadRng, seq::IteratorRandom as _};
use subnetworks_assignations::MembershipHandler;
use thiserror::Error;
use tokio::{
    sync::mpsc::{self, UnboundedSender},
    time::{Duration, sleep},
};
use tokio_stream::wrappers::{BroadcastStream, UnboundedReceiverStream};

use crate::{
    SubnetworkId,
    addressbook::AddressBookHandler,
    protocol::SAMPLING_PROTOCOL,
    protocols::sampling::{
        SubnetsConfig,
        errors::{HistoricCommitmentsError, HistoricSamplingError, SamplingError},
        historic::HistoricSamplingEvent,
        opinions::{Opinion, OpinionEvent},
        streams::{self, SampleStream},
    },
    swarm::validator::{CommitmentsArgs, SampleArgs},
};

const MAX_PEER_RETRIES: usize = 5;
const CONNECTION_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Error)]
enum StreamSamplingError {
    #[error("Connection timed out")]
    Timeout,
    #[error("Sampling error occurred: {0}")]
    SamplingError(#[from] SamplingError),
}

type HistoricSamplingResponseSuccess = (
    HeaderId,
    HashMap<BlobId, Vec<DaLightShare>>,
    HashMap<BlobId, DaSharesCommitments>,
    OpinionEvent,
);

type HistoricFutureError = (HeaderId, HistoricSamplingError);

// Future that includes context with the original sampling future result
type HistoricSamplingResponseFuture =
    BoxFuture<'static, Result<HistoricSamplingResponseSuccess, HistoricFutureError>>;

type HistoricCommitmentsResponseSuccess = (HeaderId, BlobId, DaSharesCommitments, OpinionEvent);

type HistoricCommitmentsResponseFuture = BoxFuture<
    'static,
    Result<HistoricCommitmentsResponseSuccess, (HeaderId, HistoricCommitmentsError)>,
>;

/// Historic sampling protocol that uses membership snapshots
/// Takes care of sending sampling requests using provided historic membership
pub struct HistoricRequestSamplingBehaviour<Membership, Addressbook>
where
    Membership: MembershipHandler,
    Addressbook: AddressBookHandler,
{
    /// Self peer id
    local_peer_id: PeerId,
    /// Underlying stream behaviour
    stream_behaviour: libp2p_stream::Behaviour,
    /// Underlying stream control
    control: Control,
    /// Pending sampling stream tasks with their context
    historic_request_tasks: FuturesUnordered<HistoricSamplingResponseFuture>,
    /// Pending commitments stream tasks with their context
    historic_commitments_tasks: FuturesUnordered<HistoricCommitmentsResponseFuture>,
    /// Addressbook used for getting addresses of peers
    addressbook: Addressbook,
    /// Pending samples for historic requests sender
    historic_request_sender: UnboundedSender<SampleArgs<Membership>>,
    /// Pending samples for historic requests stream
    historic_request_stream: BoxStream<'static, SampleArgs<Membership>>,
    /// Pending historic commitments requests sender
    historic_commitments_request_sender: UnboundedSender<CommitmentsArgs<Membership>>,
    /// Pending historic commitments for historic requests stream
    historic_commitments_request_stream: BoxStream<'static, CommitmentsArgs<Membership>>,
    /// Subnets sampling config that is used when picking new subnetwork peers
    subnets_config: SubnetsConfig,
    /// Broadcast channel for notifying about established connections
    connection_broadcast_sender: tokio::sync::broadcast::Sender<PeerId>,
    /// Rewards opinion events
    opinion_events: VecDeque<OpinionEvent>,
}

impl<Membership, Addressbook> HistoricRequestSamplingBehaviour<Membership, Addressbook>
where
    Membership: MembershipHandler + Clone + Send + Sync + 'static,
    Membership::NetworkId: Send,
    Addressbook: AddressBookHandler + 'static,
{
    pub fn new(
        local_peer_id: PeerId,
        addressbook: Addressbook,
        subnets_config: SubnetsConfig,
    ) -> Self {
        let stream_behaviour = libp2p_stream::Behaviour::new();
        let control = stream_behaviour.new_control();

        let stream_tasks = FuturesUnordered::new();
        let commitments_tasks = FuturesUnordered::new();

        let (historic_request_sender, receiver) = mpsc::unbounded_channel();
        let historic_request_stream = UnboundedReceiverStream::new(receiver).boxed();

        let (historic_commitments_request_sender, commitments_receiver) = mpsc::unbounded_channel();
        let historic_commitments_request_stream =
            UnboundedReceiverStream::new(commitments_receiver).boxed();

        let (connection_broadcast_sender, _) = tokio::sync::broadcast::channel(1024);
        let opinion_events = VecDeque::new();

        Self {
            local_peer_id,
            stream_behaviour,
            control,
            historic_request_tasks: stream_tasks,
            historic_commitments_tasks: commitments_tasks,
            addressbook,
            historic_request_sender,
            historic_request_stream,
            historic_commitments_request_sender,
            historic_commitments_request_stream,
            subnets_config,
            connection_broadcast_sender,
            opinion_events,
        }
    }

    // Open the stream and wait for connection
    async fn open_stream(
        peer_id: PeerId,
        mut control: Control,
        connection_receiver: tokio::sync::broadcast::Receiver<PeerId>,
    ) -> Result<SampleStream, StreamSamplingError> {
        let stream_result = control.open_stream(peer_id, SAMPLING_PROTOCOL).await;

        match stream_result {
            Ok(stream) => Ok(SampleStream { stream, peer_id }),
            Err(OpenStreamError::Io(io_err)) if io_err.kind() == ErrorKind::ConnectionReset => {
                let timer = sleep(CONNECTION_WAIT_TIMEOUT);
                tokio::pin!(timer);

                let mut connection_stream = BroadcastStream::new(connection_receiver)
                    .filter(|result| ready(matches!(result, Ok(pid) if *pid == peer_id)));

                tokio::select! {
                    Some(_) = connection_stream.next() => {
                        // Move on to retry
                    }
                    () = &mut timer => {
                        return Err(StreamSamplingError::Timeout);
                    }
                }

                // Retry once more after connection is established
                let stream = control
                    .open_stream(peer_id, SAMPLING_PROTOCOL)
                    .await
                    .map_err(|error| SamplingError::OpenStream { peer_id, error })?;

                Ok(SampleStream { stream, peer_id })
            }
            Err(error) => Err(StreamSamplingError::SamplingError(
                SamplingError::OpenStream { peer_id, error },
            )),
        }
    }

    /// Get a hook to the sender channel for historic sampling requests
    pub fn historic_request_channel(&self) -> UnboundedSender<SampleArgs<Membership>> {
        self.historic_request_sender.clone()
    }

    /// Get a hook to the sender channel for historic commitments requests
    pub fn historic_commitments_request_channel(
        &self,
    ) -> UnboundedSender<CommitmentsArgs<Membership>> {
        self.historic_commitments_request_sender.clone()
    }
}

impl<Membership, Addressbook> HistoricRequestSamplingBehaviour<Membership, Addressbook>
where
    Membership:
        MembershipHandler<Id = PeerId, NetworkId = SubnetworkId> + Clone + Send + Sync + 'static,
    Addressbook: AddressBookHandler<Id = PeerId> + Send + Sync + 'static,
{
    fn sample_historic(&self, sample_args: SampleArgs<Membership>) {
        let (block_id, blobs_by_membership) = sample_args;
        let control = self.control.clone();
        let local_peer_id = self.local_peer_id;
        let connection_broadcast_sender = self.connection_broadcast_sender.clone();
        let num_of_subnets = self.subnets_config.num_of_subnets;

        let request_future = async move {
            let mut membership_tasks = FuturesUnordered::new();

            // Spawn a sampling task for each membership
            for (membership, blob_ids) in blobs_by_membership {
                // Pre-compute subnets before entering async block to avoid Send issues
                let subnets: Vec<SubnetworkId> = {
                    let mut rng = rand::rng();
                    (0..membership.last_subnetwork_id()).choose_multiple(&mut rng, num_of_subnets)
                };

                let control = control.clone();
                let connection_broadcast_sender = connection_broadcast_sender.clone();

                let task = async move {
                    let mut opinions = Vec::new();

                    let (commitments, opinion_event) = Self::sample_all_commitments(
                        &membership,
                        &subnets,
                        &local_peer_id,
                        &blob_ids,
                        &control,
                        connection_broadcast_sender.subscribe(),
                    )
                    .await?;

                    opinions.extend(opinion_event.opinions);

                    let (shares, opinion_event) = Self::sample_all_shares(
                        &subnets,
                        &membership,
                        &local_peer_id,
                        &blob_ids,
                        &control,
                        &connection_broadcast_sender,
                    )
                    .await?;

                    opinions.extend(opinion_event.opinions);

                    Ok::<_, HistoricSamplingError>((shares, commitments, opinions))
                };

                membership_tasks.push(task);
            }

            // Collect and merge all results
            let mut all_shares: HashMap<BlobId, Vec<DaLightShare>> = HashMap::new();
            let mut all_commitments: HashMap<BlobId, DaSharesCommitments> = HashMap::new();
            let mut all_opinions = Vec::new();

            while let Some(result) = membership_tasks.next().await {
                match result {
                    Ok((shares, commitments, opinions)) => {
                        for (blob_id, share_vec) in shares {
                            all_shares.entry(blob_id).or_default().extend(share_vec);
                        }

                        // should be one per blob
                        all_commitments.extend(commitments);
                        all_opinions.extend(opinions);
                    }
                    Err(err) => {
                        // If any membership sampling fails, the whole block sampling fails
                        return Err((block_id, err));
                    }
                }
            }

            Ok((
                block_id,
                all_shares,
                all_commitments,
                OpinionEvent {
                    opinions: all_opinions,
                },
            ))
        }
        .boxed();

        self.historic_request_tasks.push(request_future);
    }

    fn sample_commitments_only(&self, commitments_args: CommitmentsArgs<Membership>) {
        let (block_id, membership, blob_id) = commitments_args;
        let control = self.control.clone();
        let local_peer_id = self.local_peer_id;
        let connection_broadcast_sender = self.connection_broadcast_sender.clone();
        let num_of_subnets = self.subnets_config.num_of_subnets;

        let request_future = async move {
            let subnets: Vec<SubnetworkId> = {
                let mut rng = rand::rng();
                (0..membership.last_subnetwork_id()).choose_multiple(&mut rng, num_of_subnets)
            };

            let blob_ids = HashSet::from([blob_id]); // Convert single blob to HashSet for reuse

            let (commitments_map, opinion_event) = Self::sample_all_commitments(
                &membership,
                &subnets,
                &local_peer_id,
                &blob_ids,
                &control,
                connection_broadcast_sender.subscribe(),
            )
            .await
            .map_err(|err| (block_id, err.into()))?;

            // Extract the single commitment from the map
            let commitment = commitments_map
                .get(&blob_id)
                .ok_or_else(|| {
                    (
                        block_id,
                        HistoricCommitmentsError::CommitmentsFailed(opinion_event.clone()),
                    )
                })?
                .clone();

            Ok((block_id, blob_id, commitment, opinion_event))
        }
        .boxed();

        self.historic_commitments_tasks.push(request_future);
    }
    async fn sample_all_shares(
        subnets: &[SubnetworkId],
        membership: &Membership,
        local_peer_id: &PeerId,
        blob_ids: &HashSet<BlobId>,
        control: &Control,
        connection_sender: &tokio::sync::broadcast::Sender<PeerId>,
    ) -> Result<(HashMap<BlobId, Vec<DaLightShare>>, OpinionEvent), HistoricSamplingError> {
        let mut subnetwork_tasks = FuturesUnordered::new();
        let session_id = membership.session_id();

        for subnetwork_id in subnets {
            let task = Self::sample_shares_for_subnetwork(
                membership,
                local_peer_id,
                control.clone(),
                blob_ids.clone(),
                *subnetwork_id,
                connection_sender.subscribe(),
                session_id,
            );
            subnetwork_tasks.push(task);
        }

        let mut all_shares = HashMap::new();
        let mut opinions = Vec::new();
        while let Some(result) = subnetwork_tasks.next().await {
            match result {
                Ok((shares, opinion_event)) => {
                    for (blob_id, share_vec) in shares {
                        all_shares
                            .entry(blob_id)
                            .or_insert_with(Vec::new)
                            .extend(share_vec);
                    }

                    opinions.extend(opinion_event.opinions);
                }
                Err(err) => return Err(err),
            }
        }

        Ok((all_shares, OpinionEvent { opinions }))
    }

    async fn sample_shares_for_subnetwork(
        membership: &Membership,
        local_peer_id: &PeerId,
        control: Control,
        mut blob_ids: HashSet<BlobId>,
        subnetwork_id: SubnetworkId,
        connection_receiver: tokio::sync::broadcast::Receiver<PeerId>,
        session_id: SessionNumber,
    ) -> Result<(HashMap<BlobId, Vec<DaLightShare>>, OpinionEvent), HistoricSamplingError> {
        let candidate_peers = {
            let mut rng = rand::rng();
            Self::pick_random_subnetwork_peers(subnetwork_id, membership, local_peer_id, &mut rng)
        };

        let mut all_shares = HashMap::new();
        let mut opinions = Vec::new();

        'peers_loop: for peer_id in candidate_peers {
            if blob_ids.is_empty() {
                break;
            }

            match Self::open_stream(peer_id, control.clone(), connection_receiver.resubscribe())
                .await
            {
                Ok(mut stream) => {
                    let blob_ids_to_try: Vec<BlobId> = blob_ids.iter().copied().collect();

                    for blob_id in blob_ids_to_try {
                        let request = sampling::SampleRequest::new_share(blob_id, subnetwork_id);
                        match Self::execute_sample_request(
                            stream,
                            request,
                            try_extract_share_data,
                            peer_id,
                            session_id,
                            &mut opinions,
                        )
                        .await
                        {
                            Ok((share_data, new_stream)) => {
                                all_shares
                                    .entry(blob_id)
                                    .or_insert_with(Vec::new)
                                    .push(share_data);
                                blob_ids.remove(&blob_id);
                                stream = new_stream;
                            }
                            Err(()) => continue 'peers_loop,
                        }
                    }
                    drop(stream.stream.close().await);
                }
                Err(err) => {
                    log::error!("Failed to open stream to peer {peer_id}: {err}");
                    opinions.push(Opinion::Negative {
                        peer_id,
                        session_id,
                    });
                }
            }
        }

        let opinion_event = OpinionEvent { opinions };
        if blob_ids.is_empty() {
            Ok((all_shares, opinion_event))
        } else {
            Err(HistoricSamplingError::SamplingFailed(opinion_event))
        }
    }

    async fn sample_all_commitments(
        membership: &Membership,
        subnets: &[SubnetworkId],
        local_peer_id: &PeerId,
        blob_ids: &HashSet<BlobId>,
        control: &Control,
        connection_receiver: tokio::sync::broadcast::Receiver<PeerId>,
    ) -> Result<(HashMap<BlobId, DaSharesCommitments>, OpinionEvent), HistoricSamplingError> {
        let session_id = membership.session_id();

        let candidate_peers = {
            let mut peers = Vec::new();
            let mut rng = rand::rng();

            for _ in 0..MAX_PEER_RETRIES {
                if let Some(subnet) = subnets.iter().choose(&mut rng)
                    && let Some(peer) =
                        Self::pick_subnetwork_peer(*subnet, membership, local_peer_id, &mut rng)
                {
                    peers.push(peer);
                }
            }
            peers
        };

        if candidate_peers.is_empty() {
            return Err(HistoricSamplingError::InternalServerError(
                "No peers available for commitments".to_owned(),
            ));
        }

        let mut remaining_blob_ids = blob_ids.clone();
        let mut commitments = HashMap::new();
        let mut opinions = Vec::new();

        'peers_loop: for peer_id in candidate_peers {
            if remaining_blob_ids.is_empty() {
                break;
            }

            match Self::open_stream(peer_id, control.clone(), connection_receiver.resubscribe())
                .await
            {
                Ok(mut stream) => {
                    let blob_ids_to_try: Vec<BlobId> = remaining_blob_ids.iter().copied().collect();

                    for blob_id in blob_ids_to_try {
                        let request = sampling::SampleRequest::new_commitments(blob_id);

                        match Self::execute_sample_request(
                            stream,
                            request,
                            try_extract_commitments,
                            peer_id,
                            session_id,
                            &mut opinions,
                        )
                        .await
                        {
                            Ok((comm, new_stream)) => {
                                commitments.insert(blob_id, comm);
                                remaining_blob_ids.remove(&blob_id);
                                stream = new_stream;
                            }
                            Err(()) => continue 'peers_loop,
                        }
                    }
                    drop(stream.stream.close().await);
                }
                Err(err) => {
                    log::error!("Failed to open stream to peer {peer_id}: {err}");
                    opinions.push(Opinion::Negative {
                        peer_id,
                        session_id,
                    });
                }
            }
        }

        let opinion_event = OpinionEvent { opinions };
        if remaining_blob_ids.is_empty() {
            Ok((commitments, opinion_event))
        } else {
            Err(HistoricSamplingError::SamplingFailed(opinion_event))
        }
    }

    #[inline]
    async fn execute_sample_request<T, F>(
        stream: SampleStream,
        request: sampling::SampleRequest,
        response_extractor: F,
        peer_id: PeerId,
        session_id: SessionNumber,
        opinions: &mut Vec<Opinion>,
    ) -> Result<(T, SampleStream), ()>
    where
        F: FnOnce(SampleResponse) -> Option<T>,
    {
        match streams::stream_sample(stream, request).await {
            Ok((_, response, mut new_stream)) => {
                if let Some(data) = response_extractor(response) {
                    opinions.push(Opinion::Positive {
                        peer_id,
                        session_id,
                    });
                    Ok((data, new_stream))
                } else {
                    opinions.push(Opinion::Negative {
                        peer_id,
                        session_id,
                    });
                    drop(new_stream.stream.close().await);
                    Err(())
                }
            }
            Err((sampling_error, maybe_stream)) => {
                if let Some(opinion) =
                    Self::classify_sampling_error_opinion(&sampling_error, peer_id, session_id)
                {
                    opinions.push(opinion);
                }
                if let Some(mut s) = maybe_stream {
                    drop(s.stream.close().await);
                }
                Err(())
            }
        }
    }

    const fn classify_sampling_error_opinion(
        sampling_error: &SamplingError,
        peer_id: PeerId,
        session_id: SessionNumber,
    ) -> Option<Opinion> {
        match sampling_error {
            SamplingError::InvalidBlobId { .. } | SamplingError::Deserialize { .. } => {
                Some(Opinion::Blacklist {
                    peer_id,
                    session_id,
                })
            }
            SamplingError::Io { .. }
            | SamplingError::Share { .. }
            | SamplingError::Commitments { .. }
            | SamplingError::OpenStream { .. }
            | SamplingError::RequestChannel { .. }
            | SamplingError::BlobNotFound { .. }
            | SamplingError::ResponseChannel { .. }
            | SamplingError::MismatchSubnetwork { .. } => Some(Opinion::Negative {
                peer_id,
                session_id,
            }),
            SamplingError::NoSubnetworkPeers { .. }
            | SamplingError::MismatchSession { .. }
            | SamplingError::CommitmentsMismatchSession { .. } => None,
        }
    }

    fn pick_subnetwork_peer(
        subnetwork_id: SubnetworkId,
        membership: &Membership,
        local_peer_id: &PeerId,
        rng: &mut ThreadRng,
    ) -> Option<PeerId> {
        let candidates = membership.members_of(&subnetwork_id);
        candidates
            .into_iter()
            .filter(|peer| peer != local_peer_id)
            .choose(rng)
    }

    fn pick_random_subnetwork_peers(
        subnetwork_id: SubnetworkId,
        membership: &Membership,
        local_peer_id: &PeerId,
        rng: &mut ThreadRng,
    ) -> Vec<PeerId> {
        let candidates = membership.members_of(&subnetwork_id);
        let available_peers: Vec<_> = candidates
            .into_iter()
            .filter(|peer| peer != local_peer_id)
            .collect();

        available_peers
            .into_iter()
            .choose_multiple(rng, MAX_PEER_RETRIES)
    }

    fn poll_historic_tasks(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Option<Poll<ToSwarm<<Self as NetworkBehaviour>::ToSwarm, THandlerInEvent<Self>>>> {
        if let Poll::Ready(Some(future_result)) = self.historic_request_tasks.poll_next_unpin(cx) {
            cx.waker().wake_by_ref();
            match future_result {
                Ok((block_id, shares, commitments, opinion_event)) => {
                    // Queue the opinion event for emission
                    if !opinion_event.opinions.is_empty() {
                        self.opinion_events.push_back(opinion_event);
                    }
                    return Some(Self::handle_historic_success(block_id, shares, commitments));
                }
                Err((block_id, sampling_error)) => {
                    // Extract opinions from error if it contains them
                    if let HistoricSamplingError::SamplingFailed(opinion_event) = &sampling_error
                        && !opinion_event.opinions.is_empty()
                    {
                        self.opinion_events.push_back(opinion_event.clone());
                    }
                    return Some(Self::handle_historic_error(block_id, sampling_error));
                }
            }
        }
        None
    }

    fn poll_historic_commitments_tasks(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Option<Poll<ToSwarm<<Self as NetworkBehaviour>::ToSwarm, THandlerInEvent<Self>>>> {
        if let Poll::Ready(Some(future_result)) =
            self.historic_commitments_tasks.poll_next_unpin(cx)
        {
            cx.waker().wake_by_ref();
            match future_result {
                Ok((block_id, blob_id, commitments, opinion_event)) => {
                    if !opinion_event.opinions.is_empty() {
                        self.opinion_events.push_back(opinion_event);
                    }
                    return Some(Poll::Ready(ToSwarm::GenerateEvent(
                        HistoricSamplingEvent::CommitmentsSuccess {
                            block_id,
                            blob_id,
                            commitments,
                        },
                    )));
                }
                Err((block_id, commitments_error)) => {
                    if let HistoricCommitmentsError::CommitmentsFailed(opinion_event) =
                        &commitments_error
                        && !opinion_event.opinions.is_empty()
                    {
                        self.opinion_events.push_back(opinion_event.clone());
                    }
                    return Some(Self::handle_commitments_error(block_id, commitments_error));
                }
            }
        }
        None
    }

    const fn handle_historic_success(
        block_id: HeaderId,
        shares: HashMap<BlobId, Vec<DaLightShare>>,
        commitments: HashMap<BlobId, DaSharesCommitments>,
    ) -> Poll<ToSwarm<<Self as NetworkBehaviour>::ToSwarm, THandlerInEvent<Self>>> {
        Poll::Ready(ToSwarm::GenerateEvent(
            HistoricSamplingEvent::SamplingSuccess {
                block_id,
                commitments,
                shares,
            },
        ))
    }

    const fn handle_historic_error(
        block_id: HeaderId,
        sampling_error: HistoricSamplingError,
    ) -> Poll<ToSwarm<<Self as NetworkBehaviour>::ToSwarm, THandlerInEvent<Self>>> {
        match sampling_error {
            HistoricSamplingError::SamplingFailed(_)
            | HistoricSamplingError::InternalServerError(_) => Poll::Ready(ToSwarm::GenerateEvent(
                HistoricSamplingEvent::SamplingError {
                    block_id,
                    error: sampling_error,
                },
            )),
        }
    }

    const fn handle_commitments_error(
        block_id: HeaderId,
        sampling_error: HistoricCommitmentsError,
    ) -> Poll<ToSwarm<<Self as NetworkBehaviour>::ToSwarm, THandlerInEvent<Self>>> {
        match sampling_error {
            HistoricCommitmentsError::CommitmentsFailed(_)
            | HistoricCommitmentsError::InternalServerError(_) => Poll::Ready(
                ToSwarm::GenerateEvent(HistoricSamplingEvent::CommitmentsError {
                    block_id,
                    error: sampling_error,
                }),
            ),
        }
    }
}

impl<Membership, Addressbook> NetworkBehaviour
    for HistoricRequestSamplingBehaviour<Membership, Addressbook>
where
    Membership:
        MembershipHandler<Id = PeerId, NetworkId = SubnetworkId> + Clone + Send + Sync + 'static,
    Addressbook: AddressBookHandler<Id = PeerId> + Send + Sync + 'static,
{
    type ConnectionHandler = <libp2p_stream::Behaviour as NetworkBehaviour>::ConnectionHandler;
    type ToSwarm = HistoricSamplingEvent;

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        let _ = self.connection_broadcast_sender.send(peer);

        self.stream_behaviour.handle_established_inbound_connection(
            connection_id,
            peer,
            local_addr,
            remote_addr,
        )
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        role_override: Endpoint,
        port_use: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        let _ = self.connection_broadcast_sender.send(peer);

        self.stream_behaviour
            .handle_established_outbound_connection(
                connection_id,
                peer,
                addr,
                role_override,
                port_use,
            )
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        self.stream_behaviour.on_swarm_event(event);
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        self.stream_behaviour
            .on_connection_handler_event(peer_id, connection_id, event);
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        //poll opinion events
        if let Some(opinion_event) = self.opinion_events.pop_front() {
            cx.waker().wake_by_ref();
            return Poll::Ready(ToSwarm::GenerateEvent(HistoricSamplingEvent::Opinion(
                opinion_event,
            )));
        }

        // Poll pending historic sampling requests
        if let Poll::Ready(Some(sample_args)) = self.historic_request_stream.poll_next_unpin(cx) {
            self.sample_historic(sample_args);
        }

        // Poll pending historic commitments-only requests
        if let Poll::Ready(Some(commitments_args)) =
            self.historic_commitments_request_stream.poll_next_unpin(cx)
        {
            self.sample_commitments_only(commitments_args);
        }

        // poll stream tasks
        if let Some(result) = self.poll_historic_tasks(cx) {
            return result;
        }

        // Poll commitments-only tasks
        if let Some(result) = self.poll_historic_commitments_tasks(cx) {
            return result;
        }

        // Handle underlying stream behaviour
        if let Poll::Ready(ToSwarm::Dial { mut opts }) = self.stream_behaviour.poll(cx)
            && let Some(address) = opts
                .get_peer_id()
                .and_then(|peer_id: PeerId| self.addressbook.get_address(&peer_id))
        {
            opts = DialOpts::peer_id(opts.get_peer_id().unwrap())
                .addresses(vec![address])
                .extend_addresses_through_behaviour()
                .build();
            cx.waker().wake_by_ref();
            return Poll::Ready(ToSwarm::Dial { opts });
        }

        Poll::Pending
    }
}

#[inline]
fn try_extract_share_data(response: SampleResponse) -> Option<DaLightShare> {
    if let SampleResponse::Share(share_data) = response {
        Some(share_data.data)
    } else {
        None
    }
}

#[inline]
fn try_extract_commitments(response: SampleResponse) -> Option<DaSharesCommitments> {
    if let SampleResponse::Commitments(comm) = response {
        Some(comm)
    } else {
        None
    }
}
