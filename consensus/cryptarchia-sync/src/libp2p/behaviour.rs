use core::future::ready;
use std::{
    collections::HashSet,
    mem,
    task::{Context, Poll},
};

use educe::Educe;
use futures::{AsyncWriteExt as _, FutureExt as _, StreamExt as _, future::BoxFuture};
use lb_core::header::HeaderId;
use lb_log_targets::cryptarchia;
use libp2p::{
    Multiaddr, PeerId, Stream as Libp2pStream, Stream, StreamProtocol,
    core::{Endpoint, transport::PortUse},
    futures::stream::FuturesUnordered,
    swarm::{
        ConnectionDenied, ConnectionHandler, ConnectionId, FromSwarm, NetworkBehaviour,
        THandlerInEvent, ToSwarm,
    },
};
use libp2p_stream::{Behaviour as StreamBehaviour, Control, IncomingStreams};
use tokio::sync::{mpsc, mpsc::Sender, oneshot};
use tracing::{debug, error, warn};

use crate::{
    BlocksResponse, DownloadBlocksRequest, TipResponse,
    config::Config,
    libp2p::{
        behaviour::old_era::OldEra,
        downloader::Downloader,
        errors::ChainSyncError,
        messages::RequestMessage,
        provider::{MAX_ADDITIONAL_BLOCKS, Provider, ReceivingRequestStream},
    },
    messages::{GetTipResponse, SerialisedBlock},
};

mod old_era;

type SendingBlocksRequestsFuture = BoxFuture<'static, Result<BlocksRequestStream, ChainSyncError>>;

type SendingTipRequestFuture = BoxFuture<'static, Result<TipRequestStream, ChainSyncError>>;

type ReceivingBlocksResponsesFuture = BoxFuture<'static, Result<(), ChainSyncError>>;

type ReceivingTipResponsesFuture = BoxFuture<'static, Result<(), ChainSyncError>>;

type SendingBlocksResponsesFuture = BoxFuture<'static, Result<(), ChainSyncError>>;

type SendingTipResponsesFuture = BoxFuture<'static, Result<(), ChainSyncError>>;

type ReceivingRequestsFuture = BoxFuture<'static, Result<ReceivingRequestStream, ChainSyncError>>;

const LOG_TARGET: &str = cryptarchia::sync::libp2p::REQUESTS;

pub type BoxedStream<T> = Box<dyn futures::Stream<Item = T> + Send + Unpin>;

type ToSwarmEvent = ToSwarm<
    <Behaviour as NetworkBehaviour>::ToSwarm,
    <<Behaviour as NetworkBehaviour>::ConnectionHandler as ConnectionHandler>::FromBehaviour,
>;

pub struct BlocksRequestStream {
    pub peer_id: PeerId,
    pub stream: Libp2pStream,
    pub reply_channel: oneshot::Sender<BoxedStream<Result<SerialisedBlock, ChainSyncError>>>,
}

impl BlocksRequestStream {
    pub const fn new(
        peer_id: PeerId,
        stream: Stream,
        reply_channel: oneshot::Sender<BoxedStream<Result<SerialisedBlock, ChainSyncError>>>,
    ) -> Self {
        Self {
            peer_id,
            stream,
            reply_channel,
        }
    }
}

pub struct TipRequestStream {
    pub peer_id: PeerId,
    pub stream: Libp2pStream,
    pub reply_channel: oneshot::Sender<Result<GetTipResponse, ChainSyncError>>,
}

impl TipRequestStream {
    pub const fn new(
        peer_id: PeerId,
        stream: Stream,
        reply_channel: oneshot::Sender<Result<GetTipResponse, ChainSyncError>>,
    ) -> Self {
        Self {
            peer_id,
            stream,
            reply_channel,
        }
    }
}

#[derive(Clone, Educe)]
#[educe(Debug)]
pub enum Event {
    ProvideBlocksRequest {
        /// Return blocks up to `target_block`.
        target_block: HeaderId,
        /// The local canonical chain latest block.
        local_tip: HeaderId,
        /// The latest immutable block.
        latest_immutable_block: HeaderId,
        /// The list of additional blocks that the requester has.
        additional_blocks: HashSet<HeaderId>,
        /// Channel to send blocks to the service.
        #[educe(Debug(ignore))]
        reply_sender: Sender<BlocksResponse>,
    },
    ProvideTipsRequest {
        /// Channel to send the latest tip to the service.
        #[educe(Debug(ignore))]
        reply_sender: Sender<TipResponse>,
    },
}

pub struct Behaviour {
    /// The stream behavior to handle actual networking.
    stream_behaviour: StreamBehaviour,
    /// The chain sync over the protocol spoken now.
    sync_protocol_state: SyncProtocolState,
    /// The chain sync over the protocol of the era before, until the end of
    /// its transition period.
    old_era: Option<OldEra>,
    /// Configuration for the behaviour.
    config: Config,
}

/// The chain sync over one protocol: the streams of the protocol it accepts,
/// and the requests and responses under way on it.
struct SyncProtocolState {
    /// Control to open streams to peers.
    control: Control,
    /// A handle to listen to incoming stream requests.
    incoming_streams: IncomingStreams,
    /// The requests and responses under way on the protocol.
    ongoing_requests: OngoingRequests,
    /// Protocol name.
    protocol_name: StreamProtocol,
}

/// The requests and responses under way on one protocol, in either direction.
struct OngoingRequests {
    /// Futures for reading incoming requests. This is common to both tip and
    /// block because initially we don't know which request we receive over
    /// stream. After reading the request, we use dedicated `FuturesUnordered`,
    /// either `sending_block_responses` or `sending_tip_responses`.
    receiving_requests: FuturesUnordered<ReceivingRequestsFuture>,
    /// Futures for sending block download requests. After the request is
    /// read, reading blocks are handled by `receiving_block_responses`.
    sending_block_requests: FuturesUnordered<SendingBlocksRequestsFuture>,
    /// Futures for managing the progress of locally initiated block downloads.
    receiving_block_responses: FuturesUnordered<ReceivingBlocksResponsesFuture>,
    /// Futures for managing the progress of externally initiated block
    /// downloads.
    sending_block_responses: FuturesUnordered<SendingBlocksResponsesFuture>,
    /// Futures for sending tip requests. After the request is
    /// read, the reading tip is handled by `receiving_tip_responses`.
    sending_tip_requests: FuturesUnordered<SendingTipRequestFuture>,
    /// Futures for managing the progress of locally initiated tip requests.
    receiving_tip_responses: FuturesUnordered<ReceivingTipResponsesFuture>,
    /// Futures for managing the progress of externally initiated tip
    /// requests.
    sending_tip_responses: FuturesUnordered<SendingTipResponsesFuture>,
    /// Futures for closing rejected incoming streams. Only pushed to by
    /// `reject_incoming_stream`.
    incoming_streams_to_close: FuturesUnordered<BoxFuture<'static, ()>>,
    /// Waker to notify the behaviour when `request_tip` or
    /// `start_blocks_download` is called.
    waker: Option<std::task::Waker>,
    /// Configuration for the behaviour.
    config: Config,
}

impl Behaviour {
    #[must_use]
    pub fn new(protocol_name: StreamProtocol, config: Config) -> Self {
        let stream_behaviour = StreamBehaviour::new();
        let sync_protocol_state = SyncProtocolState::new(
            stream_behaviour.new_control(),
            protocol_name,
            config.clone(),
        );
        Self {
            stream_behaviour,
            sync_protocol_state,
            old_era: None,
            config,
        }
    }

    /// Starts a new era, whose chain sync speaks `new_era_protocol_name`.
    ///
    /// The chain sync of the era before becomes the old era until
    /// [`Self::finish_era_transition`]: its protocol is refused from now on, but
    /// the requests and responses already under way on it run to completion,
    /// and every request started from now on goes over the new protocol. An old
    /// era still open by then is ended first: its transition period is over,
    /// and only the report of its end went missing.
    pub fn start_new_era(&mut self, new_era_protocol_name: StreamProtocol) {
        if new_era_protocol_name == self.sync_protocol_state.protocol_name {
            return;
        }
        if self.old_era.is_some() {
            warn!(
                target: LOG_TARGET,
                "The end of the previous era's transition period was never reported; ending it now"
            );
        }
        self.finish_era_transition();
        let previous_sync_state = mem::replace(
            &mut self.sync_protocol_state,
            SyncProtocolState::new(
                self.stream_behaviour.new_control(),
                new_era_protocol_name,
                self.config.clone(),
            ),
        );
        // Switch to the new sync protocol right away by notifying the waker to poll the
        // swarm.
        previous_sync_state.ongoing_requests.try_notify_waker();
        self.old_era = Some(OldEra::new(previous_sync_state));
    }

    /// Ends the transition period of the old era: the requests and responses
    /// still under way on its protocol are dropped. The connections stay open,
    /// since the new era's chain sync and the other protocols run over them
    /// too.
    pub fn finish_era_transition(&mut self) {
        self.old_era = None;
    }

    pub fn request_tip(
        &self,
        peer_id: PeerId,
        reply_sender: oneshot::Sender<Result<GetTipResponse, ChainSyncError>>,
    ) -> Result<(), ChainSyncError> {
        self.sync_protocol_state.request_tip(peer_id, reply_sender);
        Ok(())
    }

    pub fn start_blocks_download(
        &self,
        peer_id: PeerId,
        target_block: HeaderId,
        local_tip: HeaderId,
        latest_immutable_block: HeaderId,
        additional_blocks: HashSet<HeaderId>,
        reply_sender: oneshot::Sender<BoxedStream<Result<SerialisedBlock, ChainSyncError>>>,
    ) -> Result<(), ChainSyncError> {
        self.sync_protocol_state.start_blocks_download(
            peer_id,
            target_block,
            local_tip,
            latest_immutable_block,
            additional_blocks,
            reply_sender,
        );
        Ok(())
    }
}

impl SyncProtocolState {
    fn new(mut control: Control, protocol_name: StreamProtocol, config: Config) -> Self {
        let incoming_streams = control
            .accept(protocol_name.clone())
            .expect("Failed to accept incoming streams for sync protocol");
        Self {
            control,
            incoming_streams,
            ongoing_requests: OngoingRequests::new(config),
            protocol_name,
        }
    }

    fn request_tip(
        &self,
        peer_id: PeerId,
        reply_sender: oneshot::Sender<Result<GetTipResponse, ChainSyncError>>,
    ) {
        let mut control = self.control.clone();
        let protocol_name = self.protocol_name.clone();

        self.ongoing_requests.sending_tip_requests.push(
            async move {
                Downloader::send_tip_request(peer_id, &mut control, protocol_name, reply_sender)
                    .await
            }
            .boxed(),
        );

        self.ongoing_requests.try_notify_waker();
    }

    fn start_blocks_download(
        &self,
        peer_id: PeerId,
        target_block: HeaderId,
        local_tip: HeaderId,
        latest_immutable_block: HeaderId,
        additional_blocks: HashSet<HeaderId>,
        reply_sender: oneshot::Sender<BoxedStream<Result<SerialisedBlock, ChainSyncError>>>,
    ) {
        let control = self.control.clone();
        let request = DownloadBlocksRequest::new(
            target_block,
            local_tip,
            latest_immutable_block,
            additional_blocks,
        );
        let protocol_name = self.protocol_name.clone();

        self.ongoing_requests.sending_block_requests.push(
            Downloader::send_download_request(
                peer_id,
                control,
                request,
                protocol_name,
                reply_sender,
            )
            .boxed(),
        );

        self.ongoing_requests.try_notify_waker();
    }

    /// Polls the requests and responses under way, then the streams opened
    /// over the protocol.
    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<ToSwarmEvent> {
        if let Poll::Ready(event) = self.ongoing_requests.poll(cx) {
            return Poll::Ready(event);
        }

        if let Poll::Ready(Some((peer_id, stream))) = self.incoming_streams.poll_next_unpin(cx) {
            self.ongoing_requests
                .handle_incoming_stream(peer_id, stream);
        }

        Poll::Pending
    }
}

impl OngoingRequests {
    fn new(config: Config) -> Self {
        Self {
            receiving_block_responses: FuturesUnordered::new(),
            sending_block_responses: FuturesUnordered::new(),
            receiving_requests: FuturesUnordered::new(),
            incoming_streams_to_close: FuturesUnordered::new(),
            sending_block_requests: FuturesUnordered::new(),
            sending_tip_requests: FuturesUnordered::new(),
            receiving_tip_responses: FuturesUnordered::new(),
            sending_tip_responses: FuturesUnordered::new(),
            waker: None,
            config,
        }
    }

    fn handle_tip_request(&self, peer_id: PeerId, stream: Libp2pStream) -> Poll<ToSwarmEvent> {
        let (reply_sender, reply_receiver) = mpsc::channel(1);

        self.sending_tip_responses.push(
            async move { Provider::provide_tip(reply_receiver, peer_id, stream).await }.boxed(),
        );

        Poll::Ready(ToSwarm::GenerateEvent(Event::ProvideTipsRequest {
            reply_sender,
        }))
    }

    fn handle_download_request(
        &self,
        peer_id: PeerId,
        request: DownloadBlocksRequest,
        stream: Libp2pStream,
    ) -> Poll<ToSwarmEvent> {
        let (reply_sender, reply_receiver) = mpsc::channel(1);

        self.sending_block_responses.push(
            async move { Provider::provide_blocks(reply_receiver, peer_id, stream).await }.boxed(),
        );

        Poll::Ready(ToSwarm::GenerateEvent(Event::ProvideBlocksRequest {
            target_block: request.target_block,
            local_tip: request.known_blocks.local_tip,
            latest_immutable_block: request.known_blocks.latest_immutable_block,
            additional_blocks: request.known_blocks.additional_blocks,
            reply_sender,
        }))
    }

    fn handle_incoming_stream(&self, peer_id: PeerId, stream: Libp2pStream) {
        // Only requests still being read are limited here. Once the request type
        // is known, `admit_request` applies the per-type limits.
        if self.receiving_requests.len() >= self.config.max_inbound_requests.get() {
            self.reject_incoming_stream(stream, "too many pending incoming requests");
        } else {
            self.receiving_requests.push(
                Provider::process_request(peer_id, stream, self.config.peer_response_timeout)
                    .boxed(),
            );
        }

        self.try_notify_waker();
    }

    fn handle_tip_request_available(&self, request_stream: TipRequestStream) {
        self.receiving_tip_responses.push(
            Downloader::receive_tip(request_stream, self.config.peer_response_timeout).boxed(),
        );

        self.try_notify_waker();
    }

    fn handle_blocks_request_available(&self, request_stream: BlocksRequestStream) {
        self.receiving_block_responses.push(
            ready(Downloader::receive_blocks(
                request_stream,
                self.config.peer_response_timeout,
            ))
            .boxed(),
        );

        self.try_notify_waker();
    }

    fn handle_request_ready(
        &self,
        receive_request_stream: ReceivingRequestStream,
    ) -> Poll<ToSwarmEvent> {
        let (peer_id, stream, request) = receive_request_stream;

        if let Err(reason) = self.admit_request(&request) {
            self.reject_incoming_stream(stream, reason);
            return Poll::Pending;
        }

        let event = match request {
            RequestMessage::DownloadBlocksRequest(request) => {
                self.handle_download_request(peer_id, request, stream)
            }
            RequestMessage::GetTip => self.handle_tip_request(peer_id, stream),
        };

        self.try_notify_waker();

        event
    }

    /// Decides whether a fully read inbound request should be served.
    fn admit_request(&self, request: &RequestMessage) -> Result<(), &'static str> {
        match request {
            RequestMessage::DownloadBlocksRequest(request) => {
                if self.sending_block_responses.len() >= self.config.max_inbound_requests.get() {
                    return Err("too many concurrent download requests");
                }
                if request.known_blocks.additional_blocks.len() > MAX_ADDITIONAL_BLOCKS {
                    return Err("excessive number of additional blocks");
                }
                Ok(())
            }
            // Tip requests are cheap to serve, so they are never rejected.
            RequestMessage::GetTip => Ok(()),
        }
    }

    fn reject_incoming_stream(&self, mut stream: Libp2pStream, reason: &str) {
        error!(target: LOG_TARGET, "Rejected incoming request: {reason}");

        self.incoming_streams_to_close.push(
            async move {
                drop(stream.close().await);
            }
            .boxed(),
        );

        self.try_notify_waker();
    }

    fn try_notify_waker(&self) {
        if let Some(waker) = &self.waker {
            waker.wake_by_ref();
        }
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: address this in a dedicated refactor"
    )]
    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<ToSwarmEvent> {
        self.waker = Some(cx.waker().clone());

        if self.incoming_streams_to_close.poll_next_unpin(cx) == Poll::Ready(Some(())) {
            debug!(target: LOG_TARGET, "Incoming stream closed");
        }

        if let Poll::Ready(Some(result)) = self.sending_block_requests.poll_next_unpin(cx) {
            match result {
                Ok(request_stream) => {
                    self.handle_blocks_request_available(request_stream);
                }
                Err(err) => {
                    error!(target: LOG_TARGET, %err, "failed to send a block download request");
                }
            }

            return Poll::Pending;
        }

        if let Poll::Ready(Some(result)) = self.sending_tip_requests.poll_next_unpin(cx) {
            match result {
                Ok(request_stream) => {
                    self.handle_tip_request_available(request_stream);
                }
                Err(err) => {
                    error!(target: LOG_TARGET, %err, "failed to send a tip request");
                }
            }

            return Poll::Pending;
        }

        if let Poll::Ready(Some(_)) = self.receiving_block_responses.poll_next_unpin(cx) {
            return Poll::Pending;
        }

        if let Poll::Ready(Some(_)) = self.receiving_tip_responses.poll_next_unpin(cx) {
            return Poll::Pending;
        }

        if let Poll::Ready(Some(result)) = self.sending_block_responses.poll_next_unpin(cx) {
            if let Err(e) = result {
                error!(target: LOG_TARGET, "Sending response failed: {}", e);
            }

            self.try_notify_waker();

            return Poll::Pending;
        }

        if let Poll::Ready(Some(result)) = self.sending_tip_responses.poll_next_unpin(cx) {
            if let Err(e) = result {
                error!(target: LOG_TARGET, "Sending response failed: {}", e);
            }

            self.try_notify_waker();
            return Poll::Pending;
        }

        if let Poll::Ready(Some(result)) = self.receiving_requests.poll_next_unpin(cx) {
            match result {
                Ok(request_stream) => return self.handle_request_ready(request_stream),
                Err(e) => {
                    error!(target: LOG_TARGET, "Error while processing incoming request: {}", e);
                }
            }
        }

        Poll::Pending
    }
}

impl NetworkBehaviour for Behaviour {
    type ConnectionHandler = <StreamBehaviour as NetworkBehaviour>::ConnectionHandler;
    type ToSwarm = Event;

    fn handle_pending_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<(), ConnectionDenied> {
        self.stream_behaviour.handle_pending_inbound_connection(
            connection_id,
            local_addr,
            remote_addr,
        )
    }

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer_id: PeerId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        self.stream_behaviour.handle_established_inbound_connection(
            connection_id,
            peer_id,
            local_addr,
            remote_addr,
        )
    }

    fn handle_pending_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        maybe_peer: Option<PeerId>,
        addresses: &[Multiaddr],
        effective_role: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        self.stream_behaviour.handle_pending_outbound_connection(
            connection_id,
            maybe_peer,
            addresses,
            effective_role,
        )
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer_id: PeerId,
        addr: &Multiaddr,
        role_override: Endpoint,
        port: PortUse,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        self.stream_behaviour
            .handle_established_outbound_connection(
                connection_id,
                peer_id,
                addr,
                role_override,
                port,
            )
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        self.stream_behaviour.on_swarm_event(event);
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        conn_id: ConnectionId,
        event: <Self::ConnectionHandler as ConnectionHandler>::ToBehaviour,
    ) {
        self.stream_behaviour
            .on_connection_handler_event(peer_id, conn_id, event);
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        if let Poll::Ready(event) = self.sync_protocol_state.poll(cx) {
            return Poll::Ready(event);
        }

        if let Some(old_era) = &mut self.old_era
            && let Poll::Ready(event) = old_era.poll(cx)
        {
            return Poll::Ready(event);
        }

        if let Poll::Ready(ToSwarm::Dial { opts }) = self.stream_behaviour.poll(cx) {
            // If we dial, some outgoing task is created, poll again.
            cx.waker().wake_by_ref();
            return Poll::Ready(ToSwarm::Dial { opts });
        }

        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, iter, time::Duration};

    use futures::StreamExt as _;
    use lb_core::header::HeaderId;
    use lb_cryptarchia_engine::Slot;
    use libp2p::{Multiaddr, PeerId, StreamProtocol, Swarm, bytes::Bytes, swarm::SwarmEvent};
    use libp2p_swarm_test::SwarmExt as _;
    use rand::{Rng as _, thread_rng};
    use tokio::sync::{mpsc, oneshot};

    use crate::{
        BlocksResponse, BlocksUnavailableReason, DynError,
        GetTipResponse::Tip,
        GetTipResponseReason, ProviderResponse, TipResponse,
        config::Config,
        libp2p::{
            behaviour::{Behaviour, BoxedStream, Event},
            errors::{ChainSyncError, ChainSyncErrorKind},
            provider::MAX_ADDITIONAL_BLOCKS,
        },
        messages::{GetTipResponse, SerialisedBlock},
    };

    #[tokio::test]
    async fn test_block_sync_between_two_swarms() {
        let config = Config {
            peer_response_timeout: Duration::from_secs(1),
            max_inbound_requests: 4.try_into().unwrap(),
        };
        let (mut downloader_swarm, provider_peer_id) =
            start_provider_and_downloader(200, config).await;

        let streams = request_download(
            &mut downloader_swarm,
            1,
            HeaderId::from([0; 32]),
            HeaderId::from([0; 32]),
            HeaderId::from([0; 32]),
            &HashSet::new(),
            provider_peer_id,
        );

        tokio::spawn(async move { downloader_swarm.loop_on_next().await });

        let (blocks, errors) = wait_block_messages(streams).await;

        assert_eq!(blocks.len(), 200);
        assert_eq!(errors.len(), 0);
    }

    #[tokio::test]
    async fn test_reject_excess_download_requests() {
        let config = Config {
            peer_response_timeout: Duration::from_secs(1),
            max_inbound_requests: 4.try_into().unwrap(),
        };
        let (mut downloader_swarm, provider_peer_id) =
            start_provider_and_downloader(1, config.clone()).await;

        let streams = request_download(
            &mut downloader_swarm,
            config.max_inbound_requests.get() + 1,
            HeaderId::from([0; 32]),
            HeaderId::from([0; 32]),
            HeaderId::from([0; 32]),
            &HashSet::new(),
            provider_peer_id,
        );

        tokio::spawn(async move { downloader_swarm.loop_on_next().await });

        let (blocks, errors) = wait_block_messages(streams).await;

        assert_eq!(blocks.len(), config.max_inbound_requests.get());
        assert_eq!(errors.len(), 1);
    }

    #[tokio::test]
    async fn test_reject_protocol_violation_too_many_additional_blocks() {
        let config = Config {
            peer_response_timeout: Duration::from_secs(1),
            max_inbound_requests: 4.try_into().unwrap(),
        };
        let (mut downloader_swarm, provider_peer_id) =
            start_provider_and_downloader(1, config).await;

        let streams = request_download(
            &mut downloader_swarm,
            1,
            HeaderId::from([0; 32]),
            HeaderId::from([0; 32]),
            HeaderId::from([0; 32]),
            &(0..=MAX_ADDITIONAL_BLOCKS)
                .map(|i| HeaderId::from([i as u8; 32]))
                .collect::<HashSet<HeaderId>>(),
            provider_peer_id,
        );

        tokio::spawn(async move { downloader_swarm.loop_on_next().await });

        let (_blocks, errors) = wait_block_messages(streams).await;
        assert_eq!(errors.len(), 1);
    }

    #[tokio::test]
    async fn test_get_tip() {
        let config = Config {
            peer_response_timeout: Duration::from_secs(1),
            max_inbound_requests: 4.try_into().unwrap(),
        };
        let (mut downloader_swarm, provider_peer_id) =
            start_provider_and_downloader(0, config).await;

        let receiver = request_tip(&mut downloader_swarm, provider_peer_id);

        tokio::spawn(async move { downloader_swarm.loop_on_next().await });

        let Tip { tip, slot, height } = receiver.await.unwrap().unwrap() else {
            panic!("Expected a tip response");
        };

        assert_eq!(tip, HeaderId::from([0; 32]));
        assert_eq!(slot, Slot::from(0));
        assert_eq!(height, 0);
    }

    #[tokio::test]
    async fn test_get_tip_while_download_requests_are_full() {
        let config = Config {
            peer_response_timeout: Duration::from_secs(5),
            max_inbound_requests: 4.try_into().unwrap(),
        };
        let (provider_swarm, provider_peer_id, provider_addr) =
            setup_provider_swarm(config.clone());

        // Never answer download requests so they keep occupying all slots, but
        // report each one as it arrives.
        let (download_tx, mut download_rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut provider_swarm = provider_swarm;
            #[expect(
                clippy::collection_is_never_read,
                reason = "Holding the reply senders keeps the download requests pending."
            )]
            let mut pending_downloads = Vec::new();
            while let Some(event) = provider_swarm.next().await {
                match event {
                    SwarmEvent::Behaviour(Event::ProvideTipsRequest { reply_sender }) => {
                        reply_sender
                            .send(StandardProvider { blocks_count: 0 }.handle_tip_request())
                            .await
                            .unwrap();
                    }
                    SwarmEvent::Behaviour(Event::ProvideBlocksRequest { reply_sender, .. }) => {
                        pending_downloads.push(reply_sender);
                        download_tx.send(()).unwrap();
                    }
                    _ => {}
                }
            }
        });

        let mut downloader_swarm =
            setup_downloader_and_connect(provider_addr, config.clone()).await;
        let streams = request_download(
            &mut downloader_swarm,
            config.max_inbound_requests.get(),
            HeaderId::from([0; 32]),
            HeaderId::from([0; 32]),
            HeaderId::from([0; 32]),
            &HashSet::new(),
            provider_peer_id,
        );
        let tip_receiver = request_tip(&mut downloader_swarm, provider_peer_id);
        let excess_streams = request_download(
            &mut downloader_swarm,
            1,
            HeaderId::from([0; 32]),
            HeaderId::from([0; 32]),
            HeaderId::from([0; 32]),
            &HashSet::new(),
            provider_peer_id,
        );

        tokio::spawn(async move { downloader_swarm.loop_on_next().await });

        tokio::time::timeout(Duration::from_secs(10), async move {
            // Exactly `max_inbound_requests` downloads reach the provider.
            for _ in 0..config.max_inbound_requests.get() {
                download_rx.recv().await.unwrap();
            }

            let Tip { tip, .. } = tip_receiver.await.unwrap().unwrap() else {
                panic!("Expected a tip response");
            };
            assert_eq!(tip, HeaderId::from([0; 32]));

            let (blocks, errors) = wait_block_messages(excess_streams).await;
            assert_eq!(blocks.len(), 0);
            assert_eq!(errors.len(), 1);
            assert!(download_rx.try_recv().is_err());
        })
        .await
        .expect("test timed out");

        drop(streams);
    }

    #[tokio::test]
    async fn test_timeout() {
        let config = Config {
            peer_response_timeout: Duration::from_secs(1),
            max_inbound_requests: 4.try_into().unwrap(),
        };
        let mut provider_swarm = new_swarm_with_quic(config.clone());
        let provider_swarm_peer_id = *provider_swarm.local_peer_id();

        let provider_addr: Multiaddr = format!(
            "/ip4/127.0.0.1/udp/{}/quic-v1",
            thread_rng().gen_range(10000..60000)
        )
        .parse()
        .unwrap();

        provider_swarm.listen_on(provider_addr.clone()).unwrap();

        tokio::spawn(async move {
            while let Some(event) = provider_swarm.next().await {
                if let SwarmEvent::Behaviour(Event::ProvideBlocksRequest { .. }) = event {
                    tokio::time::sleep(Duration::from_secs(100)).await;
                }
            }
        });

        let mut downloader_swarm = new_swarm_with_quic(config);
        downloader_swarm.dial_and_wait(provider_addr).await;

        let streams = request_download(
            &mut downloader_swarm,
            1,
            HeaderId::from([0; 32]),
            HeaderId::from([0; 32]),
            HeaderId::from([0; 32]),
            &HashSet::new(),
            provider_swarm_peer_id,
        );

        tokio::spawn(async move { downloader_swarm.loop_on_next().await });

        let (_blocks, errors) = wait_block_messages(streams).await;

        assert!(matches!(errors[0].kind, ChainSyncErrorKind::Timeout(_)));
    }

    #[tokio::test]
    async fn test_tip_request_rejection() {
        let config = Config {
            peer_response_timeout: Duration::from_secs(1),
            max_inbound_requests: 4.try_into().unwrap(),
        };
        let (mut downloader_swarm, provider_peer_id) = start_rejecting_provider(config).await;

        let receiver = request_tip(&mut downloader_swarm, provider_peer_id);

        tokio::spawn(async move { downloader_swarm.loop_on_next().await });

        let result = receiver.await.unwrap();
        let err = result.unwrap_err();
        assert!(matches!(err.kind, ChainSyncErrorKind::RequestTipError(_)));
    }

    #[tokio::test]
    async fn test_block_request_rejection() {
        let config = Config {
            peer_response_timeout: Duration::from_secs(1),
            max_inbound_requests: 4.try_into().unwrap(),
        };
        let (mut downloader_swarm, provider_peer_id) = start_rejecting_provider(config).await;

        let streams = request_download(
            &mut downloader_swarm,
            1,
            HeaderId::from([0; 32]),
            HeaderId::from([0; 32]),
            HeaderId::from([0; 32]),
            &HashSet::new(),
            provider_peer_id,
        );

        tokio::spawn(async move { downloader_swarm.loop_on_next().await });

        let (blocks, errors) = wait_block_messages(streams).await;

        assert_eq!(blocks.len(), 0);
        assert_eq!(errors.len(), 1);
    }

    #[tokio::test]
    async fn test_block_stream_error_during_transmission() {
        let config = Config {
            peer_response_timeout: Duration::from_secs(1),
            max_inbound_requests: 4.try_into().unwrap(),
        };
        let (mut downloader_swarm, provider_peer_id) =
            start_provider_with_stream_error(config).await;

        let streams = request_download(
            &mut downloader_swarm,
            1,
            HeaderId::from([0; 32]),
            HeaderId::from([0; 32]),
            HeaderId::from([0; 32]),
            &HashSet::new(),
            provider_peer_id,
        );

        tokio::spawn(async move { downloader_swarm.loop_on_next().await });

        let (blocks, errors) = wait_block_messages(streams).await;

        assert_eq!(blocks.len(), 1);
        assert_eq!(errors.len(), 1);
    }

    #[tokio::test]
    async fn test_new_era_over_open_connection() {
        let config = Config {
            peer_response_timeout: Duration::from_secs(1),
            max_inbound_requests: 4.try_into().unwrap(),
        };
        let (mut provider_swarm, provider_peer_id, mut downloader_swarm) =
            connect_provider_and_downloader(config).await;

        // Both peers start a new era, with a new protocol, after they
        // connected.
        let next_protocol = StreamProtocol::new("/tests/chain-sync/next");
        provider_swarm
            .behaviour_mut()
            .start_new_era(next_protocol.clone());
        downloader_swarm
            .behaviour_mut()
            .start_new_era(next_protocol);

        tokio::spawn(run_provider(
            provider_swarm,
            StandardProvider { blocks_count: 10 },
        ));

        let streams = request_download(
            &mut downloader_swarm,
            1,
            HeaderId::from([0; 32]),
            HeaderId::from([0; 32]),
            HeaderId::from([0; 32]),
            &HashSet::new(),
            provider_peer_id,
        );

        tokio::spawn(async move { downloader_swarm.loop_on_next().await });

        let (blocks, errors) =
            tokio::time::timeout(Duration::from_secs(10), wait_block_messages(streams))
                .await
                .expect("The download should go over the open connection");

        assert_eq!(blocks.len(), 10);
        assert_eq!(errors.len(), 0);
    }

    #[tokio::test]
    async fn test_old_era_request_under_way_is_served_during_the_transition() {
        let config = Config {
            peer_response_timeout: Duration::from_secs(1),
            max_inbound_requests: 4.try_into().unwrap(),
        };
        let (mut provider_swarm, provider_peer_id, mut downloader_swarm) =
            connect_provider_and_downloader(config).await;

        // The provider starts a new era once the request has reached it, and
        // only then answers it.
        tokio::spawn(async move {
            while let Some(event) = provider_swarm.next().await {
                if let SwarmEvent::Behaviour(Event::ProvideBlocksRequest { reply_sender, .. }) =
                    event
                {
                    provider_swarm
                        .behaviour_mut()
                        .start_new_era(StreamProtocol::new("/tests/chain-sync/next"));
                    let response = StandardProvider { blocks_count: 10 }.handle_blocks_request(0);
                    tokio::spawn(async move {
                        reply_sender.send(response).await.unwrap();
                    });
                }
            }
        });

        let streams = request_download(
            &mut downloader_swarm,
            1,
            HeaderId::from([0; 32]),
            HeaderId::from([0; 32]),
            HeaderId::from([0; 32]),
            &HashSet::new(),
            provider_peer_id,
        );

        tokio::spawn(async move { downloader_swarm.loop_on_next().await });

        let (blocks, errors) =
            tokio::time::timeout(Duration::from_secs(10), wait_block_messages(streams))
                .await
                .expect("The request under way should be served");

        assert_eq!(blocks.len(), 10);
        assert_eq!(errors.len(), 0);
    }

    #[tokio::test]
    async fn test_old_era_protocol_is_refused_once_a_new_era_starts() {
        let config = Config {
            peer_response_timeout: Duration::from_secs(1),
            max_inbound_requests: 4.try_into().unwrap(),
        };
        let (mut provider_swarm, provider_peer_id, mut downloader_swarm) =
            connect_provider_and_downloader(config).await;

        // Only the provider starts a new era: the downloader still speaks the
        // protocol of the era before.
        provider_swarm
            .behaviour_mut()
            .start_new_era(StreamProtocol::new("/tests/chain-sync/next"));

        tokio::spawn(run_provider(
            provider_swarm,
            StandardProvider { blocks_count: 10 },
        ));

        let streams = request_download(
            &mut downloader_swarm,
            1,
            HeaderId::from([0; 32]),
            HeaderId::from([0; 32]),
            HeaderId::from([0; 32]),
            &HashSet::new(),
            provider_peer_id,
        );

        tokio::spawn(async move { downloader_swarm.loop_on_next().await });

        let (blocks, errors) = wait_block_messages(streams).await;

        assert_eq!(blocks.len(), 0);
        assert_eq!(errors.len(), 1);
    }

    /// A provider and a downloader connected to each other, neither of them
    /// polled since.
    async fn connect_provider_and_downloader(
        config: Config,
    ) -> (Swarm<Behaviour>, PeerId, Swarm<Behaviour>) {
        let (mut provider_swarm, provider_peer_id, provider_addr) =
            setup_provider_swarm(config.clone());
        provider_swarm.add_external_address(provider_addr);

        let mut downloader_swarm = new_swarm_with_quic(config);
        downloader_swarm.connect(&mut provider_swarm).await;

        (provider_swarm, provider_peer_id, downloader_swarm)
    }

    async fn setup_downloader_and_connect(
        provider_addr: Multiaddr,
        config: Config,
    ) -> Swarm<Behaviour> {
        let mut downloader_swarm = new_swarm_with_quic(config);
        downloader_swarm.dial_and_wait(provider_addr).await;
        downloader_swarm
    }

    trait ProviderBehavior: Send + 'static {
        fn handle_tip_request(&self) -> TipResponse;
        fn handle_blocks_request(&self, blocks_count: usize) -> BlocksResponse;
    }

    struct StandardProvider {
        blocks_count: usize,
    }

    impl ProviderBehavior for StandardProvider {
        fn handle_tip_request(&self) -> TipResponse {
            ProviderResponse::Available(Tip {
                tip: HeaderId::from([0; 32]),
                slot: Slot::from(0),
                height: 0,
            })
        }

        fn handle_blocks_request(&self, _requested: usize) -> BlocksResponse {
            ProviderResponse::Available(
                futures::stream::iter(
                    iter::repeat_with(|| Bytes::from_static(&[0; 32]))
                        .take(self.blocks_count)
                        .map(Ok),
                )
                .boxed(),
            )
        }
    }

    struct RejectingProvider;

    impl ProviderBehavior for RejectingProvider {
        fn handle_tip_request(&self) -> TipResponse {
            ProviderResponse::Unavailable {
                reason: GetTipResponseReason::NodeNotOnline,
            }
        }

        fn handle_blocks_request(&self, _requested: usize) -> BlocksResponse {
            ProviderResponse::Unavailable {
                reason: BlocksUnavailableReason::Unknown,
            }
        }
    }

    struct ErrorStreamProvider {
        success_count: usize,
    }

    impl ProviderBehavior for ErrorStreamProvider {
        fn handle_tip_request(&self) -> TipResponse {
            ProviderResponse::Available(Tip {
                tip: HeaderId::from([0; 32]),
                slot: Slot::from(0),
                height: 0,
            })
        }

        fn handle_blocks_request(&self, _requested: usize) -> BlocksResponse {
            let success_count = self.success_count;
            ProviderResponse::Available(
                futures::stream::iter((0..50).map(move |i| {
                    if i < success_count {
                        Ok(Bytes::from_static(&[0; 32]))
                    } else {
                        Err(Box::new(std::io::Error::other("Simulated storage failure"))
                            as DynError)
                    }
                }))
                .boxed(),
            )
        }
    }

    async fn run_provider<B: ProviderBehavior>(mut provider_swarm: Swarm<Behaviour>, behavior: B) {
        while let Some(event) = provider_swarm.next().await {
            match event {
                SwarmEvent::Behaviour(Event::ProvideTipsRequest { reply_sender }) => {
                    reply_sender
                        .send(behavior.handle_tip_request())
                        .await
                        .unwrap();
                }
                SwarmEvent::Behaviour(Event::ProvideBlocksRequest { reply_sender, .. }) => {
                    let response = behavior.handle_blocks_request(0);
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        reply_sender.send(response).await.unwrap();
                    });
                }
                _ => {}
            }
        }
    }

    async fn start_provider_and_downloader(
        blocks_count: usize,
        config: Config,
    ) -> (Swarm<Behaviour>, PeerId) {
        let (provider_swarm, provider_peer_id, provider_addr) =
            setup_provider_swarm(config.clone());

        tokio::spawn(run_provider(
            provider_swarm,
            StandardProvider { blocks_count },
        ));

        let downloader_swarm = setup_downloader_and_connect(provider_addr, config).await;
        (downloader_swarm, provider_peer_id)
    }

    async fn start_rejecting_provider(config: Config) -> (Swarm<Behaviour>, PeerId) {
        let (provider_swarm, provider_peer_id, provider_addr) =
            setup_provider_swarm(config.clone());

        tokio::spawn(run_provider(provider_swarm, RejectingProvider));

        let downloader_swarm = setup_downloader_and_connect(provider_addr, config).await;
        (downloader_swarm, provider_peer_id)
    }

    async fn start_provider_with_stream_error(config: Config) -> (Swarm<Behaviour>, PeerId) {
        let (provider_swarm, provider_peer_id, provider_addr) =
            setup_provider_swarm(config.clone());

        tokio::spawn(run_provider(
            provider_swarm,
            ErrorStreamProvider { success_count: 1 },
        ));

        let downloader_swarm = setup_downloader_and_connect(provider_addr, config).await;
        (downloader_swarm, provider_peer_id)
    }

    #[cfg(test)]
    fn setup_provider_swarm(config: Config) -> (Swarm<Behaviour>, PeerId, Multiaddr) {
        let mut provider_swarm = new_swarm_with_quic(config);
        let provider_peer_id = *provider_swarm.local_peer_id();

        let provider_addr: Multiaddr = format!(
            "/ip4/127.0.0.1/udp/{}/quic-v1",
            thread_rng().gen_range(10000..60000)
        )
        .parse()
        .unwrap();

        provider_swarm.listen_on(provider_addr.clone()).unwrap();

        (provider_swarm, provider_peer_id, provider_addr)
    }

    fn request_download(
        downloader_swarm: &mut Swarm<Behaviour>,
        syncs_count: usize,
        target_block: HeaderId,
        local_tip: HeaderId,
        latest_immutable_block: HeaderId,
        additional_blocks: &HashSet<HeaderId>,
        peer_id: PeerId,
    ) -> Vec<oneshot::Receiver<BoxedStream<Result<SerialisedBlock, ChainSyncError>>>> {
        let mut channels = Vec::new();
        for _ in 0..syncs_count {
            let (tx, rx) = oneshot::channel();
            channels.push(rx);
            downloader_swarm
                .behaviour_mut()
                .start_blocks_download(
                    peer_id,
                    target_block,
                    local_tip,
                    latest_immutable_block,
                    additional_blocks.clone(),
                    tx,
                )
                .unwrap();
        }

        channels
    }

    fn request_tip(
        downloader_swarm: &mut Swarm<Behaviour>,
        peer_id: PeerId,
    ) -> oneshot::Receiver<Result<GetTipResponse, ChainSyncError>> {
        let (tx, rx) = oneshot::channel();
        downloader_swarm
            .behaviour_mut()
            .request_tip(peer_id, tx)
            .unwrap();
        rx
    }

    async fn wait_block_messages(
        streams: Vec<oneshot::Receiver<BoxedStream<Result<SerialisedBlock, ChainSyncError>>>>,
    ) -> (Vec<SerialisedBlock>, Vec<ChainSyncError>) {
        let mut blocks = Vec::new();
        let mut errors = Vec::new();

        for receiver in streams {
            let mut stream = match receiver.await {
                Ok(stream) => stream,
                Err(e) => {
                    errors.push(ChainSyncError {
                        peer: PeerId::random(),
                        kind: ChainSyncErrorKind::ChannelReceiveError(e.to_string()),
                    });
                    continue;
                }
            };

            while let Some(result) = stream.next().await {
                match result {
                    Ok(block) => {
                        blocks.push(block);
                    }
                    Err(e) => errors.push(e),
                }
            }
        }
        (blocks, errors)
    }

    fn new_swarm_with_quic(config: Config) -> Swarm<Behaviour> {
        let keypair = libp2p::identity::Keypair::generate_ed25519();
        libp2p::SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_quic()
            .with_dns()
            .unwrap()
            .with_behaviour(|_| Behaviour::new(StreamProtocol::new("/tests/chain-sync"), config))
            .unwrap()
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(10)))
            .build()
    }
}
