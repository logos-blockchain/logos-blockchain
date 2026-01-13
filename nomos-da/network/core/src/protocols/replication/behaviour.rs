use std::{
    collections::{HashMap, HashSet, VecDeque, hash_map::Entry},
    task::{Context, Poll, Waker},
    time::Duration,
};

use cached::{Cached as _, TimedSizedCache};
use futures::{
    AsyncReadExt as _, AsyncWriteExt as _, FutureExt as _, StreamExt as _,
    future::BoxFuture,
    io::{ReadHalf, WriteHalf},
    stream::{BoxStream, FuturesUnordered},
};
use indexmap::IndexMap;
use libp2p::{
    Multiaddr, PeerId, Stream,
    core::{Endpoint, transport::PortUse},
    swarm::{
        ConnectionClosed, ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler,
        THandlerInEvent, THandlerOutEvent, ToSwarm,
    },
};
use libp2p_stream::{Control, IncomingStreams, OpenStreamError};
use log::trace;
use nomos_da_messages::{
    packing::{pack_to_writer, unpack_from_reader},
    replication::{ReplicationRequest, ReplicationResponseId},
};
use nomos_utils::bounded_duration::{MINUTE, MinimalBoundedDuration};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use subnetworks_assignations::MembershipHandler;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::{SubnetworkId, protocol::REPLICATION_PROTOCOL};

#[derive(Debug, Error)]
pub enum ReplicationError {
    #[error("Stream disconnected: {error}")]
    Io {
        peer_id: PeerId,
        error: std::io::Error,
    },
    #[error("Error opening stream [{peer_id}]: {error}")]
    OpenStream {
        peer_id: PeerId,
        error: OpenStreamError,
    },
    #[error("Error dequeuing outbound message, peer not found: {peer_id}")]
    DequeueOutbound { peer_id: PeerId },
}

impl ReplicationError {
    #[must_use]
    pub const fn peer_id(&self) -> Option<&PeerId> {
        match self {
            Self::OpenStream { peer_id, .. }
            | Self::Io { peer_id, .. }
            | Self::DequeueOutbound { peer_id } => Some(peer_id),
        }
    }
}

impl Clone for ReplicationError {
    fn clone(&self) -> Self {
        match self {
            Self::Io { peer_id, error } => Self::Io {
                peer_id: *peer_id,
                error: std::io::Error::new(error.kind(), error.to_string()),
            },
            Self::OpenStream { peer_id, error } => Self::OpenStream {
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
            Self::DequeueOutbound { peer_id } => Self::DequeueOutbound { peer_id: *peer_id },
        }
    }
}

/// Nomos DA `BroadcastEvents` to be bubbled up to logic layers
#[derive(Debug)]
pub enum ReplicationEvent {
    IncomingMessage {
        peer_id: PeerId,
        message: Box<ReplicationRequest>,
    },
    ReplicationError {
        error: ReplicationError,
    },
}

impl From<ReplicationError> for ReplicationEvent {
    fn from(error: ReplicationError) -> Self {
        Self::ReplicationError { error }
    }
}

impl ReplicationEvent {
    #[must_use]
    pub fn share_size(&self) -> Option<usize> {
        match self {
            Self::IncomingMessage { message, .. } => match message.as_ref() {
                ReplicationRequest::Share(share_request) => {
                    Some(share_request.share.data.column_len())
                }
                ReplicationRequest::Tx(_) => None,
            },
            Self::ReplicationError { .. } => None,
        }
    }
}

type IncomingTask =
    BoxFuture<'static, Result<(PeerId, ReplicationRequest, ReadHalf<Stream>), ReplicationError>>;
type OutboundTask = BoxFuture<'static, Result<(PeerId, WriteHalf<Stream>), ReplicationError>>;

enum WriteHalfState {
    Idle(WriteHalf<Stream>),
    Busy,
}

impl WriteHalfState {
    pub const fn take(&mut self) -> Self {
        let mut ret = Self::Busy;
        std::mem::swap(self, &mut ret);
        ret
    }
}

/// Holds pending outbound messages for each peer and manages which message
/// should be scheduled next for writing to an outbound stream.
#[derive(Default)]
struct PendingOutbound {
    /// Pending outbound message queues for each peer
    messages: IndexMap<PeerId, VecDeque<ReplicationRequest>>,
    /// The last peer whose pending message was scheduled for sending
    last_scheduled: Option<PeerId>,
}

struct PeerNotFound;

impl PendingOutbound {
    /// Appends a message to the peer's pending queue. Lazily creates the queue
    /// for the peer upon the first call.
    fn enqueue_message(&mut self, peer_id: PeerId, message: ReplicationRequest) {
        self.messages.entry(peer_id).or_default().push_back(message);
    }

    /// Dequeues the next message from the peer's pending queue.
    fn dequeue_message(
        &mut self,
        peer_id: &PeerId,
    ) -> Result<Option<ReplicationRequest>, PeerNotFound> {
        let messages = self.messages.get_mut(peer_id).ok_or(PeerNotFound)?;
        Ok(messages.pop_front())
    }

    /// Do internal housekeeping when a peer disconnects.
    fn on_disconnected(&mut self, peer_id: &PeerId) {
        self.last_scheduled = on_disconnected(&mut self.messages, self.last_scheduled, peer_id);
    }

    /// Find the next peer whose pending message can be scheduled for writing to
    /// the peer's stream. Peers are iterated in a round-robin fashion.
    fn next_peer(&mut self, is_stream_idle_fn: impl FnMut(&PeerId) -> bool) -> Option<PeerId> {
        let next = next_peer(
            self.last_scheduled.as_ref(),
            &self.messages,
            is_stream_idle_fn,
        );
        self.last_scheduled = next;
        next
    }

    fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

#[serde_as]
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplicationConfig {
    pub seen_message_cache_size: usize,
    #[serde_as(as = "MinimalBoundedDuration<1, MINUTE>")]
    pub seen_message_ttl: Duration,
}

/// Nomos DA broadcast network behaviour.
///
/// This item handles the logic of the nomos da subnetworks broadcasting.
/// DA subnetworks are a logical distribution of subsets.
/// A node just connects and accepts connections to other nodes that are in the
/// same subsets. A node forwards messages to all connected peers which are
/// member of the addressed `SubnetworkId`.
pub struct ReplicationBehaviour<Membership> {
    /// Local peer Id, related to the libp2p public key
    local_peer_id: PeerId,
    /// Underlying stream behaviour
    stream_behaviour: libp2p_stream::Behaviour,
    /// Used to open new outbound streams from the stream behaviour
    control: Control,
    /// Provides streams that are accepted by the stream behaviour from other
    /// peers
    incoming_streams: IncomingStreams,
    /// Sends a new read half when an outbound stream initiated by us is opened
    outbound_read_half_tx: mpsc::UnboundedSender<(PeerId, ReadHalf<Stream>)>,
    /// Receives the new read half from the stream opened by us so that we can
    /// schedule reading messages from it
    outbound_read_half_rx: BoxStream<'static, (PeerId, ReadHalf<Stream>)>,
    /// Holds tasks for reading messages from streams' read halves
    incoming_tasks: FuturesUnordered<IncomingTask>,
    /// Membership handler, membership handles the subsets logics on who is
    /// where in the nomos DA subnetworks
    membership: Membership,
    /// Currently connected peers
    connected: HashSet<PeerId>,
    /// Pending outbound messages are stored here, they are then consumed by
    /// each respective stream's write half
    pending_outbound: PendingOutbound,
    /// Indicates which of the streams' write halves are currently idle or busy
    outbound_streams: HashMap<PeerId, WriteHalfState>,
    /// Holds tasks for writing messages to the streams' write halves
    outbound_tasks: FuturesUnordered<OutboundTask>,
    /// The cache for seen messages, evicts entries in two ways:
    /// - oldest entries when full (Least-Recently-Used),
    /// - after a certain TTL has passed since the entry was last accessed,
    ///   regardless of cache utilization.
    ///
    /// The cache allocates memory once up front and any expired and unused
    /// entries will eventually be overwritten and erasing them via
    /// [`TimeSizedCache::flush`] does not save space, so we don't do it.
    seen_message_cache: TimedSizedCache<ReplicationResponseId, ()>,
    /// This waker is only used when we are the initiator of
    /// [`Self::send_message`], ie. the method is called from outside the
    /// behaviour. In all other cases we wake via reference in [`Self::poll`].
    waker: Option<Waker>,
}

impl<Membership> ReplicationBehaviour<Membership> {
    pub fn new(config: ReplicationConfig, peer_id: PeerId, membership: Membership) -> Self {
        let stream_behaviour = libp2p_stream::Behaviour::new();
        let mut control = stream_behaviour.new_control();
        let incoming_streams = control
            .accept(REPLICATION_PROTOCOL)
            .expect("A unique protocol can be accepted only once");
        let incoming_tasks = FuturesUnordered::new();
        let outbound_tasks = FuturesUnordered::new();

        let (outbound_read_stream_tx, new_read_stream_rx) = mpsc::unbounded_channel();
        let outbound_read_stream_rx = UnboundedReceiverStream::new(new_read_stream_rx).boxed();

        let seen_message_cache = TimedSizedCache::with_size_and_lifespan_and_refresh(
            config.seen_message_cache_size,
            config.seen_message_ttl.as_secs(),
            // Looking up key resets its elapsed ttl
            true,
        );

        Self {
            local_peer_id: peer_id,
            stream_behaviour,
            control,
            incoming_streams,
            incoming_tasks,
            membership,
            connected: HashSet::default(),
            pending_outbound: PendingOutbound::default(),
            outbound_streams: HashMap::default(),
            outbound_tasks,
            seen_message_cache,
            waker: None,
            outbound_read_half_tx: outbound_read_stream_tx,
            outbound_read_half_rx: outbound_read_stream_rx,
        }
    }
}

impl<M> ReplicationBehaviour<M>
where
    M: MembershipHandler<NetworkId = SubnetworkId, Id = PeerId>,
{
    /// Check if some peer membership lies in at least a single subnetwork that
    /// the local peer is a member too.
    fn is_neighbour(&self, peer_id: &PeerId) -> bool {
        self.membership
            .membership(&self.local_peer_id)
            .intersection(&self.membership.membership(peer_id))
            .count()
            > 0
    }

    fn no_loopback_member_peers_of(&self, subnetwork: SubnetworkId) -> HashSet<PeerId> {
        let mut peers = self.membership.members_of(&subnetwork);
        // no loopback
        peers.remove(&self.local_peer_id);
        peers
    }

    /// Initiate sending a replication message **from outside the behaviour**
    pub fn send_message(&mut self, message: &ReplicationRequest) {
        let waker = self.waker.take();
        self.send_message_impl(waker.as_ref(), message);
    }

    /// Send a replication message to all connected peers that are members of
    /// the same subnetwork. If the message has already been seen, it is not
    /// sent again.
    fn send_message_impl(&mut self, waker: Option<&Waker>, message: &ReplicationRequest) {
        let message_id = message.id();
        if self.seen_message_cache.cache_get(&message_id).is_some() {
            return;
        }
        self.seen_message_cache.cache_set(message_id, ());

        // At least one message was enqueued
        let mut queued = false;

        match &message {
            ReplicationRequest::Share(share_request) => {
                // Push a message in the queue for every single peer connected that is a member
                // of the selected subnetwork_id
                let peers = self.no_loopback_member_peers_of(share_request.subnetwork_id);
                self.connected
                    .iter()
                    .filter(|peer_id| peers.contains(peer_id))
                    .for_each(|peer_id| {
                        self.pending_outbound
                            .enqueue_message(*peer_id, message.clone());
                        queued = true;
                    });
            }
            ReplicationRequest::Tx(_signed_mantle_tx) => {
                // Push Tx to all connected peers
                self.connected.iter().for_each(|peer_id| {
                    self.pending_outbound
                        .enqueue_message(*peer_id, message.clone());
                    queued = true;
                });
            }
        }

        if queued {
            waker.map(Waker::wake_by_ref);
        }
    }

    /// Attempt to read a single message from the stream's read half
    async fn try_read_message(
        peer_id: PeerId,
        mut read_half: ReadHalf<Stream>,
    ) -> Result<(PeerId, ReplicationRequest, ReadHalf<Stream>), ReplicationError> {
        let message = unpack_from_reader(&mut read_half)
            .await
            .map_err(|error| ReplicationError::Io { peer_id, error })?;
        Ok((peer_id, message, read_half))
    }

    /// Attempt to open a new stream from the underlying control to the peer
    async fn try_open_stream(
        peer_id: PeerId,
        mut control: Control,
        new_read_half_tx: mpsc::UnboundedSender<(PeerId, ReadHalf<Stream>)>,
    ) -> Result<WriteHalf<Stream>, ReplicationError> {
        let stream = control
            .open_stream(peer_id, REPLICATION_PROTOCOL)
            .await
            .map_err(|error| ReplicationError::OpenStream { peer_id, error })?;
        let (read_stream, write_stream) = stream.split();
        // Send the new read half to the incoming tasks so that it can be scheduled for
        // reading
        new_read_half_tx
            .send((peer_id, read_stream))
            .expect("Receiver is not dropped");
        // Write half is ready for writing messages
        Ok(write_stream)
    }

    /// Attempt to write a message to the given stream's write half
    async fn try_write_message(
        peer_id: PeerId,
        message: ReplicationRequest,
        mut write_half: WriteHalf<Stream>,
    ) -> Result<(PeerId, WriteHalf<Stream>), ReplicationError> {
        pack_to_writer(&message, &mut write_half).await.map_err(
            |error| {
                // I/O errors are expected when the receiver drops the stream (e.g. non-neighbour
                // filtering). Other errors indicate serialization bugs and should panic.
                assert!(error.kind() != std::io::ErrorKind::Other, "Message should always be serializable.\nMessage: '{message:?}'\nError: {error:?}");
                ReplicationError::Io { peer_id, error }
            },
        )?;
        write_half
            .flush()
            .await
            .map_err(|error| ReplicationError::Io { peer_id, error })?;
        Ok((peer_id, write_half))
    }

    /// Check if we've received any new messages
    fn poll_incoming_tasks(&mut self, cx: &mut Context<'_>) -> Option<ReplicationEvent>
    where
        M: 'static,
    {
        match self.incoming_tasks.poll_next_unpin(cx) {
            Poll::Ready(Some(Ok((peer_id, message, read_half)))) => {
                // For Share messages, only accept from neighbours (peers in the same
                // subnetwork). TX messages are accepted from any connected peer
                // to allow broad propagation. Receiving Share from
                // non-neighbour indicates sender misbehavior.
                if matches!(message, ReplicationRequest::Share(_)) && !self.is_neighbour(&peer_id) {
                    trace!("Dropping Share replication from non-neighbour peer {peer_id}",);
                    // Close stream by removing write half and dropping read half
                    self.outbound_streams.remove(&peer_id);
                    drop(read_half);
                    cx.waker().wake_by_ref();
                    return Some(ReplicationEvent::ReplicationError {
                        error: ReplicationError::Io {
                            peer_id,
                            error: std::io::Error::new(
                                std::io::ErrorKind::PermissionDenied,
                                "Share replication from non-neighbour",
                            ),
                        },
                    });
                }

                // Replicate the message to all connected peers from the same subnet if we
                // haven't seen it yet
                self.send_message_impl(Some(cx.waker()), &message);
                // Schedule waiting for any next incoming message on the same stream's read half
                self.incoming_tasks
                    .push(Self::try_read_message(peer_id, read_half).boxed());
                // Signal to the swarm that we've received the message but poll the other tasks
                // as well first
                Some(ReplicationEvent::IncomingMessage {
                    peer_id,
                    message: Box::new(message),
                })
            }
            Poll::Ready(Some(Err(error))) => Some(ReplicationEvent::ReplicationError { error }),
            _ => None,
        }
    }

    /// If any of the busy streams' write halves has finished sending a message,
    /// we can write the next pending message to this half if there is one
    fn poll_outbound_tasks(&mut self, cx: &mut Context<'_>) -> Result<(), ReplicationError>
    where
        M: 'static,
    {
        match self.outbound_tasks.poll_next_unpin(cx) {
            Poll::Ready(Some(Ok((peer_id, write_half)))) => {
                let message = self
                    .pending_outbound
                    .dequeue_message(&peer_id)
                    .map_err(|_| ReplicationError::DequeueOutbound { peer_id })?;

                match message {
                    Some(message) => {
                        self.outbound_tasks.push(Box::pin(Self::try_write_message(
                            peer_id, message, write_half,
                        )));

                        cx.waker().wake_by_ref();
                    }
                    None => {
                        self.outbound_streams
                            .insert(peer_id, WriteHalfState::Idle(write_half));
                    }
                }
            }
            Poll::Ready(Some(Err(error))) => {
                return Err(error);
            }
            _ => {}
        }

        Ok(())
    }

    /// Pick the next peer that has a pending message and an idle write half or
    /// an unopened stream and schedule writing a message to it.
    fn schedule_next_outbound_message(&mut self, cx: &Context<'_>) -> Result<(), ReplicationError>
    where
        M: 'static,
    {
        let next_peer = self.pending_outbound.next_peer(|id| {
            matches!(
                self.outbound_streams.get(id),
                Some(WriteHalfState::Idle(_)) | None
            )
        });

        if let Some(peer_id) = next_peer {
            let message = self
                .pending_outbound
                .dequeue_message(&peer_id)
                .map_err(|_| ReplicationError::DequeueOutbound { peer_id })?;
            let message =
                message.expect("Message is in the queue, ensured by PendingOutbound::next_peer()");

            match self.outbound_streams.entry(peer_id) {
                Entry::Occupied(mut entry) => match entry.get_mut().take() {
                    WriteHalfState::Idle(stream) => {
                        self.outbound_tasks
                            .push(Box::pin(Self::try_write_message(peer_id, message, stream)));
                    }
                    WriteHalfState::Busy => {
                        unreachable!("The stream is idle, ensured by PendingOutbound::next_peer()")
                    }
                },
                // If there is no stream for this peer yet, try to open one, and then write the
                // first message into it
                Entry::Vacant(entry) => {
                    entry.insert(WriteHalfState::Busy);
                    let control = self.control.clone();
                    // Send the read half once the stream is opened to the incoming tasks so that it
                    // can be scheduled for reading
                    let new_read_half_tx = self.outbound_read_half_tx.clone();
                    self.outbound_tasks.push(Box::pin(async move {
                        let stream =
                            Self::try_open_stream(peer_id, control, new_read_half_tx).await?;
                        let (peer_id, stream) =
                            Self::try_write_message(peer_id, message, stream).await?;
                        Ok((peer_id, stream))
                    }));
                }
            }

            cx.waker().wake_by_ref();
        }

        // We must ensure that we ge awaken if there are still pending messages in the
        // queue. In some scenarios it may happen that there's no other event that
        // triggers the wake-up.
        if !self.pending_outbound.is_empty() {
            cx.waker().wake_by_ref();
        }

        Ok(())
    }

    /// Schedule reading from any new streams initiated by other peers
    fn poll_incoming_streams(&mut self, cx: &mut Context<'_>)
    where
        M: 'static,
    {
        if let Poll::Ready(Some((peer_id, stream))) = self.incoming_streams.poll_next_unpin(cx) {
            let (read_half, write_half) = stream.split();
            // Put the write half in an idle state so that it's ready to schedule writing
            // pending messages to it
            self.outbound_streams
                .insert(peer_id, WriteHalfState::Idle(write_half));
            self.incoming_tasks
                .push(Self::try_read_message(peer_id, read_half).boxed());
            cx.waker().wake_by_ref();
        }
    }

    /// Schedule reading from any new streams initiated by us
    fn poll_new_outbound_read_halves(&mut self, cx: &mut Context<'_>)
    where
        M: 'static,
    {
        if let Poll::Ready(Some((peer_id, read_half))) =
            self.outbound_read_half_rx.poll_next_unpin(cx)
        {
            self.incoming_tasks
                .push(Self::try_read_message(peer_id, read_half).boxed());
            cx.waker().wake_by_ref();
        }
    }
}

impl<M> NetworkBehaviour for ReplicationBehaviour<M>
where
    M: MembershipHandler<NetworkId = SubnetworkId, Id = PeerId> + 'static,
{
    type ConnectionHandler = <libp2p_stream::Behaviour as NetworkBehaviour>::ConnectionHandler;
    type ToSwarm = ReplicationEvent;

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer_id: PeerId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        // No membership filtering at connection level. Neighbour filtering is done at
        // message level (in `poll_incoming_tasks`) because connection handlers are
        // immutable and can't be updated on session change when membership changes.
        trace!("{}, Connected to {peer_id}", self.local_peer_id);
        self.connected.insert(peer_id);
        self.stream_behaviour.handle_established_inbound_connection(
            connection_id,
            peer_id,
            local_addr,
            remote_addr,
        )
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer_id: PeerId,
        addr: &Multiaddr,
        role_override: Endpoint,
        port_use: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        trace!("{}, Connected to {peer_id}", self.local_peer_id);
        self.connected.insert(peer_id);
        self.stream_behaviour
            .handle_established_outbound_connection(
                connection_id,
                peer_id,
                addr,
                role_override,
                port_use,
            )
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        if let FromSwarm::ConnectionClosed(ConnectionClosed { peer_id, .. }) = event {
            self.connected.remove(&peer_id);
            self.outbound_streams.remove(&peer_id);
            self.pending_outbound.on_disconnected(&peer_id);
        }
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
        // Keep the most recent waker in case `send_message()` is called from
        // outside the behaviour
        self.waker = Some(cx.waker().clone());

        // The incoming message to be returned to the swarm **after** all the polling is
        // done, this way we don't starve the tasks that are polled later in the
        // sequence
        let mut incoming_message = None;

        match self.poll_incoming_tasks(cx) {
            Some(event @ ReplicationEvent::IncomingMessage { .. }) => {
                incoming_message = Some(Poll::Ready(ToSwarm::GenerateEvent(event)));
            }
            Some(event @ ReplicationEvent::ReplicationError { .. }) => {
                return Poll::Ready(ToSwarm::GenerateEvent(event));
            }
            None => {}
        }

        if let Err(error) = self.poll_outbound_tasks(cx) {
            return Poll::Ready(ToSwarm::GenerateEvent(ReplicationEvent::ReplicationError {
                error,
            }));
        }

        if let Err(error) = self.schedule_next_outbound_message(cx) {
            return Poll::Ready(ToSwarm::GenerateEvent(ReplicationEvent::ReplicationError {
                error,
            }));
        }

        self.poll_incoming_streams(cx);
        self.poll_new_outbound_read_halves(cx);

        if let Some(incoming_message) = incoming_message {
            return incoming_message;
        }

        Poll::Pending
    }
}

/// Find the next peer that has at least one pending message and its associated
/// stream is idle or hasn't been opened yet. Iterate over all peers, starting
/// with the one after the previous peer, wrap around and finish with the
/// previous peer at the end. This ensures we're fair scheduling one message per
/// peer at a time, iterating the peers in a round-robin fashion.
fn next_peer<K, M>(
    previous: Option<&K>,
    pending_outbound_messages: &IndexMap<K, VecDeque<M>>,
    mut is_stream_idle_fn: impl FnMut(&K) -> bool,
) -> Option<K>
where
    K: Copy + Eq + std::hash::Hash,
{
    match previous {
        Some(previous) => {
            let i = pending_outbound_messages
                .get_index_of(previous)
                .expect("Peer to be present");
            pending_outbound_messages[i + 1..]
                .iter()
                .chain(pending_outbound_messages[..=i].iter())
                .find_map(|(id, messages)| {
                    (!messages.is_empty() && is_stream_idle_fn(id)).then_some(*id)
                })
        }
        None => pending_outbound_messages.iter().find_map(|(id, messages)| {
            (!messages.is_empty() && is_stream_idle_fn(id)).then_some(*id)
        }),
    }
}

/// If the `last_scheduled` marker points to a peer that has been disconnected,
/// the marker needs to be moved back one step, so that the search for the next
/// peer in `poll()` continues where we left off last time. This function
/// returns the corrected marker value.
fn on_disconnected<P, Q>(
    messages: &mut IndexMap<P, Q>,
    last_scheduled: Option<P>,
    disconnected: &P,
) -> Option<P>
where
    P: Copy + Eq + std::hash::Hash,
{
    // Store marker index
    let i = last_scheduled.map(|id| messages.get_index_of(&id).expect("Peer to be present"));
    // Remove disconnected peer and its pending messages
    messages.shift_remove(disconnected);
    // If the marker pointed to the disconnected peer, move it backwards one step,
    // wrapping around if necessary
    let id = last_scheduled?;
    if id == *disconnected {
        let i = i.expect("Valid index");
        if i == 0 {
            messages.last().map(|(id, _)| *id)
        } else {
            messages.get_index(i - 1).map(|(id, _)| *id)
        }
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use indexmap::IndexMap;
    use rstest::rstest;

    #[rstest]
    #[case(None, vec![], None, None)]
    // One peer, previous peer is not set
    //
    // Peer does not have pending messages and the stream is busy
    #[case(None, vec![(0, vec![])], None, None)]
    // Peer does not have pending messages and the stream is idle
    #[case(None, vec![(0, vec![])], Some(0), None)]
    // Peer has pending messages and the stream is busy
    #[case(None, vec![(0, vec![()])], None, None)]
    // Peer has pending messages and the stream is idle
    #[case(None, vec![(0, vec![()])], Some(0), Some(0))]
    // One peer, previous peer is set
    //
    // Peer does not have pending messages and the stream is busy
    #[case(Some(0), vec![(0, vec![])], None, None)]
    // Peer does not have pending messages and the stream is idle
    #[case(Some(0), vec![(0, vec![])], Some(0), None)]
    // Peer has pending messages and the stream is busy
    #[case(Some(0), vec![(0, vec![()])], None, None)]
    // Peer has pending messages and the stream is idle
    #[case(Some(0), vec![(0, vec![()])], Some(0), Some(0))]
    // Multiple peers
    //
    // Advances from peer 0 to 1
    #[case(Some(0), vec![(0, vec![()]), (1, vec![()])], Some(1), Some(1))]
    // Wraps around from peer 0, via peer 1, to peer 0
    #[case(Some(0), vec![(0, vec![()]), (1, vec![()])], Some(0), Some(0))]
    // Wraps around from peer 1 to peer 0
    #[case(Some(1), vec![(0, vec![()]), (1, vec![()])], Some(0), Some(0))]
    // Wraps around from peer 1, via peer 0, to peer 1
    #[case(Some(1), vec![(0, vec![()]), (1, vec![()])], Some(1), Some(1))]
    // All streams busy
    #[case(Some(0), vec![(0, vec![()]), (1, vec![()])], None, None)]
    // All queues empty
    #[case(Some(1), vec![(0, vec![]), (1, vec![])], Some(0), None)]
    fn test_next_peer(
        #[case] previous: Option<usize>,
        #[case] pending_outbound_messages: Vec<(usize, Vec<()>)>,
        #[case] idle_stream: Option<usize>,
        #[case] expected_next: Option<usize>,
    ) {
        let pending_outbound_messages: IndexMap<usize, VecDeque<()>> = pending_outbound_messages
            .into_iter()
            .map(|(id, messages)| (id, messages.into_iter().collect()))
            .collect();
        let actual_next =
            super::next_peer(previous.as_ref(), &pending_outbound_messages, |peer_id| {
                idle_stream == Some(*peer_id)
            });
        assert_eq!(actual_next, expected_next);
    }

    #[rstest]
    /// Marker is not set, peer 0 is disconnected
    #[case(None, vec![(0, ())], 0, None, vec![])]
    /// Marker is set to peer 0, peer 0 is disconnected, marker is cleared
    #[case(Some(0), vec![(0, ())], 0, None, vec![])]
    /// Marker is set to peer 1, peer 1 is disconnected, marker is moved
    /// backwards to peer 0
    #[case(Some(0), vec![(0, ()), (1, ())], 0, Some(1), vec![(1, ())])]
    /// Marker is set to peer 0, peer 0 is disconnected, marker is moved
    /// backwards to peer 1 (wrapping around)
    #[case(Some(0), vec![(0, ()), (1, ())], 0, Some(1), vec![(1, ())])]
    fn test_on_disconnected(
        #[case] last_scheduled_peer: Option<usize>,
        #[case] pending_messages: Vec<(usize, ())>,
        #[case] disconnected_peer: usize,
        #[case] expected_last_scheduled_peer: Option<usize>,
        #[case] expected_pending_messages: Vec<(usize, ())>,
    ) {
        let mut pending_outbound_messages: IndexMap<usize, ()> =
            pending_messages.into_iter().collect();
        let actual = super::on_disconnected(
            &mut pending_outbound_messages,
            last_scheduled_peer,
            &disconnected_peer,
        );
        assert_eq!(actual, expected_last_scheduled_peer);
        assert_eq!(
            pending_outbound_messages,
            expected_pending_messages
                .into_iter()
                .collect::<IndexMap<usize, ()>>()
        );
    }
}
