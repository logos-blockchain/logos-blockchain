use std::{
    collections::{HashMap, HashSet, VecDeque},
    task::{Context, Poll, Waker},
};

use futures::{
    AsyncWriteExt as _, FutureExt as _, StreamExt as _, TryFutureExt as _,
    future::BoxFuture,
    stream::{BoxStream, FuturesUnordered},
};
use kzgrs_backend::common::share::DaShare;
use libp2p::{
    Multiaddr, PeerId, Stream,
    core::{Endpoint, transport::PortUse},
    swarm::{
        ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler, THandlerInEvent,
        THandlerOutEvent, ToSwarm, behaviour::ConnectionClosed, dial_opts::DialOpts,
    },
};
use libp2p_stream::{Control, OpenStreamError};
use nomos_core::{codec, da::BlobId, mantle::SignedMantleTx};
use nomos_da_messages::{
    dispersal,
    packing::{pack_to_writer, unpack_from_reader},
};
use subnetworks_assignations::MembershipHandler;
use thiserror::Error;
use tokio::sync::{mpsc, mpsc::UnboundedSender};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::error;

use crate::{SubnetworkId, addressbook::AddressBookHandler, protocol::DISPERSAL_PROTOCOL};

#[derive(Debug, Error)]
pub enum DispersalError {
    #[error("Stream disconnected: {error}")]
    Io {
        peer_id: PeerId,
        error: std::io::Error,
        blob_id: BlobId,
        subnetwork_id: SubnetworkId,
    },
    #[error("Could not serialized: {error}")]
    Serialization {
        error: codec::Error,
        blob_id: BlobId,
        subnetwork_id: SubnetworkId,
    },
    #[error("Dispersal response error: {error:?}")]
    Protocol {
        subnetwork_id: SubnetworkId,
        error: dispersal::DispersalError,
    },
    #[error("Message has no blob id")]
    NoBlobId,
    #[error("Error dialing peer [{peer_id}]: {error}")]
    OpenStreamError {
        peer_id: PeerId,
        error: OpenStreamError,
    },
}

impl DispersalError {
    #[must_use]
    pub const fn blob_id(&self) -> Option<BlobId> {
        match self {
            Self::Io { blob_id, .. }
            | Self::Serialization { blob_id, .. }
            | Self::Protocol {
                error: dispersal::DispersalError { blob_id, .. },
                ..
            } => Some(*blob_id),
            Self::NoBlobId | Self::OpenStreamError { .. } => None,
        }
    }

    #[must_use]
    pub const fn subnetwork_id(&self) -> Option<SubnetworkId> {
        match self {
            Self::Io { subnetwork_id, .. }
            | Self::Serialization { subnetwork_id, .. }
            | Self::Protocol { subnetwork_id, .. } => Some(*subnetwork_id),
            Self::NoBlobId | Self::OpenStreamError { .. } => None,
        }
    }

    #[must_use]
    pub const fn peer_id(&self) -> Option<&PeerId> {
        match self {
            Self::Io { peer_id, .. } | Self::OpenStreamError { peer_id, .. } => Some(peer_id),
            _ => None,
        }
    }
}

impl Clone for DispersalError {
    fn clone(&self) -> Self {
        match self {
            Self::Io {
                peer_id,
                error,
                blob_id,
                subnetwork_id,
            } => Self::Io {
                peer_id: *peer_id,
                error: std::io::Error::new(error.kind(), error.to_string()),
                blob_id: *blob_id,
                subnetwork_id: *subnetwork_id,
            },
            Self::Serialization {
                error,
                blob_id,
                subnetwork_id,
            } => Self::Serialization {
                error: error.clone(),
                blob_id: *blob_id,
                subnetwork_id: *subnetwork_id,
            },
            Self::Protocol {
                subnetwork_id,
                error,
            } => Self::Protocol {
                subnetwork_id: *subnetwork_id,
                error: error.clone(),
            },
            Self::NoBlobId => Self::NoBlobId,
            Self::OpenStreamError { peer_id, error } => Self::OpenStreamError {
                peer_id: *peer_id,
                error: match error {
                    OpenStreamError::UnsupportedProtocol(protocol) => {
                        OpenStreamError::UnsupportedProtocol(protocol.clone())
                    }
                    OpenStreamError::Io(error) => {
                        OpenStreamError::Io(std::io::Error::new(error.kind(), error.to_string()))
                    }
                    err => OpenStreamError::Io(std::io::Error::other(err.to_string())),
                },
            },
        }
    }
}

#[derive(Debug, Clone)]
pub enum DispersalExecutorEvent {
    /// A blob successfully arrived its destination
    DispersalSuccess {
        blob_id: BlobId,
        subnetwork_id: SubnetworkId,
    },
    /// Something went wrong delivering the blob
    DispersalError { error: DispersalError },
}

struct DispersalStream {
    stream: Stream,
    peer_id: PeerId,
}

type StreamHandlerFutureSuccess = (
    BlobId,
    SubnetworkId,
    dispersal::DispersalResponse,
    DispersalStream,
);
type StreamHandlerFuture = BoxFuture<'static, Result<StreamHandlerFutureSuccess, DispersalError>>;

/// Executor dispersal protocol.
///
/// It takes care of sending blobs to different subnetworks.
/// Bubbles up events with the success or error when dispersing
pub struct DispersalExecutorBehaviour<Membership, Addressbook>
where
    Membership: MembershipHandler,
    Addressbook: AddressBookHandler,
{
    /// Underlying stream behaviour
    stream_behaviour: libp2p_stream::Behaviour,
    /// Pending running tasks (one task per stream)
    tasks: FuturesUnordered<StreamHandlerFuture>,
    /// Streams which didn't have any pending task
    idle_streams: HashMap<PeerId, DispersalStream>,
    /// Subnetworks membership information
    membership: Membership,
    /// Address book handler to get addresses of peers
    addressbook: Addressbook,
    /// Pending blobs that need to be dispersed by `PeerId`
    to_disperse: HashMap<PeerId, VecDeque<(Membership::NetworkId, dispersal::DispersalRequest)>>,
    /// Pending blobs from disconnected networks
    disconnected_pending_shares:
        HashMap<Membership::NetworkId, VecDeque<dispersal::DispersalRequest>>,
    /// Already connected peers connection Ids
    connected_peers: HashMap<PeerId, ConnectionId>,
    /// List of peers that already has pending open stream request.
    pending_peer_open_stream_requests: HashSet<PeerId>,
    /// Subnetwork working streams
    subnetwork_open_streams: HashSet<SubnetworkId>,
    /// Sender hook of peers to open streams channel
    pending_out_streams_sender: UnboundedSender<PeerId>,
    /// Pending to open streams
    pending_out_streams: BoxStream<'static, Result<DispersalStream, DispersalError>>,
    /// Dispersal hook of pending blobs channel
    pending_shares_sender: UnboundedSender<(Membership::NetworkId, DaShare)>,
    /// Pending blobs stream
    pending_shares_stream: BoxStream<'static, (Membership::NetworkId, DaShare)>,
    /// Dispersal hook of pending tx channel
    pending_tx_sender: UnboundedSender<(Membership::NetworkId, SignedMantleTx)>,
    /// Pending tx stream
    pending_tx_stream: BoxStream<'static, (Membership::NetworkId, SignedMantleTx)>,
    /// Waker for dispersal pollin
    waker: Option<Waker>,
}

impl<Membership, Addressbook> DispersalExecutorBehaviour<Membership, Addressbook>
where
    Membership: MembershipHandler + 'static,
    Membership::NetworkId: Send,
    Addressbook: AddressBookHandler + 'static,
{
    pub fn new(membership: Membership, addressbook: Addressbook) -> Self {
        let stream_behaviour = libp2p_stream::Behaviour::new();
        let tasks = FuturesUnordered::new();
        let to_disperse = HashMap::new();
        let connected_peers = HashMap::new();
        let subnetwork_open_streams = HashSet::new();
        let idle_streams = HashMap::new();
        let pending_peer_open_stream_requests = HashSet::new();
        let (pending_out_streams_sender, receiver) = mpsc::unbounded_channel();
        let control = stream_behaviour.new_control();
        let pending_out_streams = UnboundedReceiverStream::new(receiver)
            .zip(futures::stream::repeat(control))
            .then(|(peer_id, control)| Self::open_stream(peer_id, control))
            .boxed();

        let (pending_shares_sender, receiver) = mpsc::unbounded_channel();
        let pending_shares_stream = UnboundedReceiverStream::new(receiver).boxed();
        let (pending_tx_sender, receiver) = mpsc::unbounded_channel();
        let pending_tx_stream = UnboundedReceiverStream::new(receiver).boxed();
        let disconnected_pending_shares = HashMap::new();

        Self {
            stream_behaviour,
            tasks,
            membership,
            addressbook,
            to_disperse,
            disconnected_pending_shares,
            connected_peers,
            subnetwork_open_streams,
            idle_streams,
            pending_peer_open_stream_requests,
            pending_out_streams_sender,
            pending_out_streams,
            pending_shares_sender,
            pending_shares_stream,
            pending_tx_sender,
            pending_tx_stream,
            waker: None,
        }
    }

    /// Open a new stream from the underlying control to the provided peer
    async fn open_stream(
        peer_id: PeerId,
        mut control: Control,
    ) -> Result<DispersalStream, DispersalError> {
        let stream = control
            .open_stream(peer_id, DISPERSAL_PROTOCOL)
            .await
            .map_err(|error| DispersalError::OpenStreamError { peer_id, error })?;
        Ok(DispersalStream { stream, peer_id })
    }

    /// Get a hook to the sender channel of open stream events
    pub fn open_stream_sender(&self) -> UnboundedSender<PeerId> {
        self.pending_out_streams_sender.clone()
    }

    /// Get a hook to the sender channel of the shares dispersal events
    pub fn shares_sender(&self) -> UnboundedSender<(Membership::NetworkId, DaShare)> {
        self.pending_shares_sender.clone()
    }

    /// Get a hook to the sender channel of the shares dispersal events
    pub fn tx_sender(&self) -> UnboundedSender<(Membership::NetworkId, SignedMantleTx)> {
        self.pending_tx_sender.clone()
    }

    /// Task for handling streams, one message at a time
    /// Writes the blob to the stream and waits for an acknowledgment response
    async fn stream_disperse(
        mut stream: DispersalStream,
        message: dispersal::DispersalRequest,
        subnetwork_id: SubnetworkId,
    ) -> Result<StreamHandlerFutureSuccess, DispersalError> {
        let blob_id = message.blob_id().ok_or(DispersalError::NoBlobId)?;
        let peer_id = stream.peer_id;
        pack_to_writer(&message, &mut stream.stream)
            .map_err(|error| DispersalError::Io {
                peer_id,
                error,
                blob_id,
                subnetwork_id,
            })
            .await?;
        stream
            .stream
            .flush()
            .await
            .map_err(|error| DispersalError::Io {
                peer_id,
                error,
                blob_id,
                subnetwork_id,
            })?;
        let response: dispersal::DispersalResponse = unpack_from_reader(&mut stream.stream)
            .await
            .map_err(|error| DispersalError::Io {
            peer_id,
            error,
            blob_id,
            subnetwork_id,
        })?;
        // `blob_id` should always be a 32bytes hash
        Ok((blob_id, subnetwork_id, response, stream))
    }

    /// Run when a stream gets free, if there is a pending task for the stream
    /// it will get scheduled to run otherwise it is parked as idle.
    fn handle_stream(
        tasks: &FuturesUnordered<StreamHandlerFuture>,
        to_disperse: &mut HashMap<PeerId, VecDeque<(SubnetworkId, dispersal::DispersalRequest)>>,
        idle_streams: &mut HashMap<PeerId, DispersalStream>,
        stream: DispersalStream,
        cx: &Context<'_>,
    ) {
        if let Some((subnetwork_id, next_request)) =
            Self::next_request(&stream.peer_id, to_disperse)
        {
            let fut = Self::stream_disperse(stream, next_request, subnetwork_id).boxed();
            tasks.push(fut);
            cx.waker().wake_by_ref();
        } else {
            // There is no pending request, so just idle the stream
            idle_streams.insert(stream.peer_id, stream);
        }
    }

    /// Get a pending request if its available
    fn next_request(
        peer_id: &PeerId,
        to_disperse: &mut HashMap<PeerId, VecDeque<(SubnetworkId, dispersal::DispersalRequest)>>,
    ) -> Option<(SubnetworkId, dispersal::DispersalRequest)> {
        to_disperse.get_mut(peer_id).and_then(VecDeque::pop_front)
    }

    pub fn try_wake(&mut self) {
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }
}

impl<Membership, Addressbook> DispersalExecutorBehaviour<Membership, Addressbook>
where
    Membership: MembershipHandler<Id = PeerId, NetworkId = SubnetworkId> + 'static,
    Addressbook: AddressBookHandler<Id = PeerId> + 'static,
{
    /// Schedule a new task for sending the blob, if stream is not available
    /// queue messages for later processing.
    fn disperse_request(
        &mut self,
        subnetwork_id: SubnetworkId,
        request: &dispersal::DispersalRequest,
    ) {
        let members = self.membership.members_of(&subnetwork_id);
        let peers = members
            .iter()
            .filter(|peer_id| self.connected_peers.contains_key(peer_id));

        // We may be connected to more than a single node. Usually will be one, but that
        // is an internal decision of the executor itself.
        for peer in peers {
            if let Some(stream) = self.idle_streams.remove(peer) {
                // push a task if the stream is immediately available
                let fut = Self::stream_disperse(stream, request.clone(), subnetwork_id).boxed();
                self.tasks.push(fut);
            } else {
                // otherwise queue the blob
                self.to_disperse
                    .entry(*peer)
                    .or_default()
                    .push_back((subnetwork_id, request.clone()));
            }
        }
    }

    fn reschedule_shares_for_peer_stream(
        stream: &DispersalStream,
        membership: &Membership,
        to_disperse: &mut HashMap<PeerId, VecDeque<(SubnetworkId, dispersal::DispersalRequest)>>,
        disconnected_pending_shares: &mut HashMap<
            SubnetworkId,
            VecDeque<dispersal::DispersalRequest>,
        >,
    ) {
        let peer_id = stream.peer_id;
        let subnetworks = membership.membership(&peer_id);
        let entry = to_disperse.entry(peer_id).or_default();
        for subnetwork in subnetworks {
            if let Some(shares) = disconnected_pending_shares.remove(&subnetwork) {
                entry.extend(shares.into_iter().map(|share| (subnetwork, share)));
            }
        }
    }

    fn try_open_stream(&mut self, subnetwork_id: SubnetworkId) {
        let members = self.membership.members_of(&subnetwork_id);
        let peers: Vec<_> = members
            .iter()
            .filter(|peer_id| self.connected_peers.contains_key(peer_id))
            .filter(|peer_id| !self.pending_peer_open_stream_requests.contains(peer_id))
            .collect();

        for peer in peers {
            if let Err(e) = self.pending_out_streams_sender.send(*peer) {
                error!("Error requesting stream for peer {peer}: {e}");
            } else {
                self.pending_peer_open_stream_requests.insert(*peer);
            }
        }
    }

    fn prune_shares_for_peer(
        &mut self,
        peer_id: PeerId,
    ) -> VecDeque<(SubnetworkId, dispersal::DispersalRequest)> {
        self.to_disperse.remove(&peer_id).unwrap_or_default()
    }

    fn recover_shares_for_disconnected_subnetworks(&mut self, peer_id: PeerId) {
        // push missing blobs into pending ones
        let disconnected_pending_shares = self.prune_shares_for_peer(peer_id);
        for (subnetwork_id, req) in disconnected_pending_shares {
            self.disconnected_pending_shares
                .entry(subnetwork_id)
                .or_default()
                .push_back(req);
        }
    }

    fn handle_connection_closed(&mut self, peer_id: PeerId) {
        if self.connected_peers.remove(&peer_id).is_some() {
            // mangle pending blobs for disconnected subnetworks from peer
            self.recover_shares_for_disconnected_subnetworks(peer_id);
        }
    }

    fn poll_pending_tasks(&mut self, cx: &mut Context<'_>) -> Option<DispersalExecutorEvent> {
        if let Poll::Ready(Some(future_result)) = self.tasks.poll_next_unpin(cx) {
            match future_result {
                Ok((blob_id, subnetwork_id, dispersal_response, stream)) => {
                    // Handle the now-free stream and return the success event.
                    Self::handle_stream(
                        &self.tasks,
                        &mut self.to_disperse,
                        &mut self.idle_streams,
                        stream,
                        cx,
                    );
                    // If the other side returned an error, propagate it.
                    if let dispersal::DispersalResponse::Error(error) = dispersal_response {
                        return Some(DispersalExecutorEvent::DispersalError {
                            error: DispersalError::Protocol {
                                subnetwork_id,
                                error,
                            },
                        });
                    }
                    Some(DispersalExecutorEvent::DispersalSuccess {
                        blob_id,
                        subnetwork_id,
                    })
                }
                // An error occurred on our side; bubble it up.
                Err(error) => Some(DispersalExecutorEvent::DispersalError { error }),
            }
        } else {
            None
        }
    }

    fn poll_pending_shares(&mut self, cx: &mut Context<'_>) {
        if let Poll::Ready(Some((subnetwork_id, share))) =
            self.pending_shares_stream.poll_next_unpin(cx)
        {
            if self.subnetwork_open_streams.contains(&subnetwork_id) {
                self.disperse_request(subnetwork_id, &dispersal::DispersalRequest::from(share));
            } else {
                self.try_open_stream(subnetwork_id);
                self.disconnected_pending_shares
                    .entry(subnetwork_id)
                    .or_default()
                    .push_back(dispersal::DispersalRequest::from(share));
            }
            cx.waker().wake_by_ref();
        }
    }

    fn poll_pending_txs(&mut self, cx: &mut Context<'_>) {
        if let Poll::Ready(Some((subnetwork_id, tx))) = self.pending_tx_stream.poll_next_unpin(cx) {
            if self.subnetwork_open_streams.contains(&subnetwork_id) {
                self.disperse_request(subnetwork_id, &dispersal::DispersalRequest::from(tx));
            } else {
                self.try_open_stream(subnetwork_id);
                self.disconnected_pending_shares
                    .entry(subnetwork_id)
                    .or_default()
                    .push_back(dispersal::DispersalRequest::from(tx));
            }
            cx.waker().wake_by_ref();
        }
    }

    fn poll_pending_streams(&mut self, cx: &mut Context<'_>) -> Option<DispersalExecutorEvent> {
        if let Poll::Ready(Some(res)) = self.pending_out_streams.poll_next_unpin(cx) {
            match res {
                Ok(stream) => {
                    self.subnetwork_open_streams
                        .extend(self.membership.membership(&stream.peer_id));
                    self.pending_peer_open_stream_requests
                        .remove(&stream.peer_id);
                    Self::reschedule_shares_for_peer_stream(
                        &stream,
                        &self.membership,
                        &mut self.to_disperse,
                        &mut self.disconnected_pending_shares,
                    );
                    Self::handle_stream(
                        &self.tasks,
                        &mut self.to_disperse,
                        &mut self.idle_streams,
                        stream,
                        cx,
                    );
                    None
                }
                Err(error) => {
                    if let Some(peer_id) = error.peer_id() {
                        self.pending_peer_open_stream_requests.remove(peer_id);
                    }
                    Some(DispersalExecutorEvent::DispersalError { error })
                }
            }
        } else {
            None
        }
    }

    fn poll_dial_requests<Out, In>(&mut self, cx: &mut Context<'_>) -> Option<ToSwarm<Out, In>> {
        if let Poll::Ready(ToSwarm::Dial { mut opts }) = self.stream_behaviour.poll(cx) {
            // Attach a known peer address if possible.
            if let Some(peer_id) = opts.get_peer_id()
                && let Some(address) = self.addressbook.get_address(&peer_id)
            {
                opts = DialOpts::peer_id(peer_id).addresses(vec![address]).build();
            }
            Some(ToSwarm::Dial { opts })
        } else {
            None
        }
    }
}

impl<Membership, Addressbook> NetworkBehaviour
    for DispersalExecutorBehaviour<Membership, Addressbook>
where
    Membership: MembershipHandler<Id = PeerId, NetworkId = SubnetworkId> + 'static,
    Addressbook: AddressBookHandler<Id = PeerId> + 'static,
{
    type ConnectionHandler = <libp2p_stream::Behaviour as NetworkBehaviour>::ConnectionHandler;
    type ToSwarm = DispersalExecutorEvent;

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        // No membership filtering at connection level. This behaviour only opens
        // outbound streams for dispersal, and peer selection is based on membership
        // at dispersal time (in `disperse_request`). Connection-level filtering would
        // cause race conditions on session change.
        self.connected_peers.insert(peer, connection_id);
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
        self.connected_peers.insert(peer, connection_id);
        if let Err(e) = self.pending_out_streams_sender.send(peer) {
            error!("Error requesting stream for peer {peer}: {e}");
        }
        self.try_wake();
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
        if let FromSwarm::ConnectionClosed(ConnectionClosed { peer_id, .. }) = event {
            self.handle_connection_closed(peer_id);
            self.try_wake();
        }
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
        self.waker = Some(cx.waker().clone());

        // Poll tasks generated by streams and share requests.
        if let Some(event) = self.poll_pending_tasks(cx) {
            return Poll::Ready(ToSwarm::GenerateEvent(event));
        }

        // Poll and process any pending shares for dispersal.
        self.poll_pending_shares(cx);

        // Poll and process any pending transactions for dispersal.
        self.poll_pending_txs(cx);

        // Poll for newly opened outbound streams.
        if let Some(event) = self.poll_pending_streams(cx) {
            return Poll::Ready(ToSwarm::GenerateEvent(event));
        }

        // Poll the underlying stream behaviour for dialing requests.
        if let Some(event) = self.poll_dial_requests(cx) {
            return Poll::Ready(event);
        }

        Poll::Pending
    }
}
