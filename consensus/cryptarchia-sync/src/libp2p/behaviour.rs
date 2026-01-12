use std::{
    collections::HashSet,
    task::{Context, Poll},
};

use futures::{AsyncWriteExt as _, FutureExt as _, StreamExt as _, future::BoxFuture};
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
use nomos_core::header::HeaderId;
use tokio::sync::{mpsc, mpsc::Sender, oneshot};
use tracing::{debug, error};

use crate::{
    BlocksResponse, DownloadBlocksRequest, TipResponse,
    config::Config,
    libp2p::{
        downloader::Downloader,
        errors::ChainSyncError,
        messages::RequestMessage,
        provider::{MAX_ADDITIONAL_BLOCKS, Provider, ReceivingRequestStream},
    },
    messages::{GetTipResponse, SerialisedBlock},
};

const MAX_INCOMING_REQUESTS: usize = 4;

type SendingBlocksRequestsFuture = BoxFuture<'static, Result<BlocksRequestStream, ChainSyncError>>;

type SendingTipRequestFuture = BoxFuture<'static, Result<TipRequestStream, ChainSyncError>>;

type ReceivingBlocksResponsesFuture = BoxFuture<'static, Result<(), ChainSyncError>>;

type ReceivingTipResponsesFuture = BoxFuture<'static, Result<(), ChainSyncError>>;

type SendingBlocksResponsesFuture = BoxFuture<'static, Result<(), ChainSyncError>>;

type SendingTipResponsesFuture = BoxFuture<'static, Result<(), ChainSyncError>>;

type ReceivingRequestsFuture = BoxFuture<'static, Result<ReceivingRequestStream, ChainSyncError>>;

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

#[derive(Debug, Clone)]
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
        reply_sender: Sender<BlocksResponse>,
    },
    ProvideTipsRequest {
        /// Channel to send the latest tip to the service.
        reply_sender: Sender<TipResponse>,
    },
}

pub struct Behaviour {
    /// The stream behavior to handle actual networking.
    stream_behaviour: StreamBehaviour,
    /// Control to open streams to peers.
    control: Control,
    /// A handle to listen to incoming stream requests.
    incoming_streams: IncomingStreams,
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
    /// Futures for closing incoming streams that were rejected due to excess
    /// requests.
    incoming_streams_to_close: FuturesUnordered<BoxFuture<'static, ()>>,
    /// Waker to notify the behaviour when `request_tip` or
    /// `start_blocks_download` is called.
    waker: Option<std::task::Waker>,
    /// Configuration for the behaviour.
    config: Config,
    /// Protocol name.
    protocol_name: StreamProtocol,
}

impl Behaviour {
    #[must_use]
    pub fn new(protocol_name: StreamProtocol, config: Config) -> Self {
        let stream_behaviour = StreamBehaviour::new();
        let mut control = stream_behaviour.new_control();
        let incoming_streams = control
            .accept(protocol_name.clone())
            .expect("Failed to accept incoming streams for sync protocol");
        Self {
            stream_behaviour,
            control,
            incoming_streams,
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
            protocol_name,
        }
    }

    pub fn request_tip(
        &self,
        peer_id: PeerId,
        reply_sender: oneshot::Sender<Result<GetTipResponse, ChainSyncError>>,
    ) -> Result<(), ChainSyncError> {
        let mut control = self.control.clone();
        let protocol_name = self.protocol_name.clone();

        self.sending_tip_requests.push(
            async move {
                Downloader::send_tip_request(peer_id, &mut control, protocol_name, reply_sender)
                    .await
            }
            .boxed(),
        );

        self.try_notify_waker();

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
        let control = self.control.clone();
        let request = DownloadBlocksRequest::new(
            target_block,
            local_tip,
            latest_immutable_block,
            additional_blocks,
        );
        let protocol_name = self.protocol_name.clone();

        self.sending_block_requests.push(
            Downloader::send_download_request(
                peer_id,
                control,
                request,
                protocol_name,
                reply_sender,
            )
            .boxed(),
        );

        self.try_notify_waker();

        Ok(())
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
        mut stream: Libp2pStream,
    ) -> Poll<ToSwarmEvent> {
        if request.known_blocks.additional_blocks.len() > MAX_ADDITIONAL_BLOCKS {
            error!("Received excessive number of additional blocks");

            self.incoming_streams_to_close.push(
                async move {
                    let _ = stream.close().await;
                }
                .boxed(),
            );

            return Poll::Pending;
        }

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

    fn handle_incoming_stream(&self, peer_id: PeerId, mut stream: Libp2pStream) {
        let concurrent_requests = self.receiving_requests.len()
            + self.sending_block_responses.len()
            + self.sending_tip_responses.len();

        if concurrent_requests >= MAX_INCOMING_REQUESTS {
            self.incoming_streams_to_close.push(
                async move {
                    let _ = stream.close().await;
                }
                .boxed(),
            );
            error!("Rejected excess pending incoming request");
        } else {
            self.receiving_requests
                .push(Provider::process_request(peer_id, stream).boxed());
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
            Downloader::receive_blocks(request_stream, self.config.peer_response_timeout).boxed(),
        );

        self.try_notify_waker();
    }

    fn handle_request_ready(
        &self,
        receive_request_stream: ReceivingRequestStream,
    ) -> Poll<
        ToSwarm<
            <Self as NetworkBehaviour>::ToSwarm,
            <<Self as NetworkBehaviour>::ConnectionHandler as ConnectionHandler>::FromBehaviour,
        >,
    > {
        let (peer_id, stream, request) = receive_request_stream;

        let event = match request {
            RequestMessage::DownloadBlocksRequest(request) => {
                self.handle_download_request(peer_id, request, stream)
            }
            RequestMessage::GetTip => self.handle_tip_request(peer_id, stream),
        };

        self.try_notify_waker();

        event
    }

    fn try_notify_waker(&self) {
        if let Some(waker) = &self.waker {
            waker.wake_by_ref();
        }
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

    #[expect(
        clippy::cognitive_complexity,
        reason = "It contains only basic polling logic"
    )]
    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        self.waker = Some(cx.waker().clone());

        if self.incoming_streams_to_close.poll_next_unpin(cx) == Poll::Ready(Some(())) {
            debug!("Incoming stream closed");
        }

        if let Poll::Ready(Some(result)) = self.sending_block_requests.poll_next_unpin(cx) {
            match result {
                Ok(request_stream) => {
                    self.handle_blocks_request_available(request_stream);
                }
                Err(e) => {
                    error!("Error while processing block download request: {}", e);
                }
            }

            return Poll::Pending;
        }

        if let Poll::Ready(Some(result)) = self.sending_tip_requests.poll_next_unpin(cx) {
            match result {
                Ok(request_stream) => {
                    self.handle_tip_request_available(request_stream);
                }
                Err(e) => {
                    error!("Error while processing tip request: {}", e);
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
                error!("Sending response failed: {}", e);
            }

            self.try_notify_waker();

            return Poll::Pending;
        }

        if let Poll::Ready(Some(result)) = self.sending_tip_responses.poll_next_unpin(cx) {
            if let Err(e) = result {
                error!("Sending response failed: {}", e);
            }

            self.try_notify_waker();
            return Poll::Pending;
        }

        if let Poll::Ready(Some(result)) = self.receiving_requests.poll_next_unpin(cx) {
            match result {
                Ok(request_stream) => return self.handle_request_ready(request_stream),
                Err(e) => {
                    error!("Error while processing incoming request: {}", e);
                }
            }
        }

        if let Poll::Ready(Some((peer_id, stream))) = self.incoming_streams.poll_next_unpin(cx) {
            self.handle_incoming_stream(peer_id, stream);

            return Poll::Pending;
        }

        if let Poll::Ready(ToSwarm::Dial { opts }) = self.stream_behaviour.poll(cx) {
            // If we dial, some outgoing task is created, poll again.
            self.try_notify_waker();
            return Poll::Ready(ToSwarm::Dial { opts });
        }

        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, iter, time::Duration};

    use cryptarchia_engine::Slot;
    use futures::StreamExt as _;
    use libp2p::{Multiaddr, PeerId, StreamProtocol, Swarm, bytes::Bytes, swarm::SwarmEvent};
    use libp2p_swarm_test::SwarmExt as _;
    use nomos_core::header::HeaderId;
    use rand::{Rng, thread_rng};
    use tokio::sync::oneshot;

    use crate::{
        BlocksResponse, DynError,
        GetTipResponse::Tip,
        ProviderResponse, TipResponse,
        config::Config,
        libp2p::{
            behaviour::{Behaviour, BoxedStream, Event, MAX_INCOMING_REQUESTS},
            errors::{ChainSyncError, ChainSyncErrorKind},
            provider::MAX_ADDITIONAL_BLOCKS,
        },
        messages::{GetTipResponse, SerialisedBlock},
    };

    #[tokio::test]
    async fn test_block_sync_between_two_swarms() {
        let (mut downloader_swarm, provider_peer_id) = start_provider_and_downloader(200).await;

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
        let (mut downloader_swarm, provider_peer_id) = start_provider_and_downloader(1).await;

        let streams = request_download(
            &mut downloader_swarm,
            MAX_INCOMING_REQUESTS + 1,
            HeaderId::from([0; 32]),
            HeaderId::from([0; 32]),
            HeaderId::from([0; 32]),
            &HashSet::new(),
            provider_peer_id,
        );

        tokio::spawn(async move { downloader_swarm.loop_on_next().await });

        let (blocks, errors) = wait_block_messages(streams).await;

        assert_eq!(blocks.len(), MAX_INCOMING_REQUESTS);
        assert_eq!(errors.len(), 1);
    }

    #[tokio::test]
    async fn test_reject_protocol_violation_too_many_additional_blocks() {
        let (mut downloader_swarm, provider_peer_id) = start_provider_and_downloader(1).await;

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
        let (mut downloader_swarm, provider_peer_id) = start_provider_and_downloader(0).await;

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
    async fn test_timeout() {
        let mut provider_swarm = new_swarm_with_quic();
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
                } else {
                    continue;
                }
            }
        });

        let mut downloader_swarm = new_swarm_with_quic();
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
        let (mut downloader_swarm, provider_peer_id) = start_rejecting_provider().await;

        let receiver = request_tip(&mut downloader_swarm, provider_peer_id);

        tokio::spawn(async move { downloader_swarm.loop_on_next().await });

        let result = receiver.await.unwrap();
        let err = result.unwrap_err();
        assert!(matches!(err.kind, ChainSyncErrorKind::RequestTipError(_)));
    }

    #[tokio::test]
    async fn test_block_request_rejection() {
        let (mut downloader_swarm, provider_peer_id) = start_rejecting_provider().await;

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
        let (mut downloader_swarm, provider_peer_id) = start_provider_with_stream_error().await;

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

    async fn setup_provider_swarm() -> (Swarm<Behaviour>, PeerId, Multiaddr) {
        let mut provider_swarm = new_swarm_with_quic();
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

    async fn setup_downloader_and_connect(provider_addr: Multiaddr) -> Swarm<Behaviour> {
        let mut downloader_swarm = new_swarm_with_quic();
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
                reason: "Node is not in online mode".to_string(),
            }
        }

        fn handle_blocks_request(&self, _requested: usize) -> BlocksResponse {
            ProviderResponse::Unavailable {
                reason: "Node is not in online mode".to_string(),
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

    async fn start_provider_and_downloader(blocks_count: usize) -> (Swarm<Behaviour>, PeerId) {
        let (provider_swarm, provider_peer_id, provider_addr) = setup_provider_swarm().await;

        tokio::spawn(run_provider(
            provider_swarm,
            StandardProvider { blocks_count },
        ));

        let downloader_swarm = setup_downloader_and_connect(provider_addr).await;
        (downloader_swarm, provider_peer_id)
    }

    async fn start_rejecting_provider() -> (Swarm<Behaviour>, PeerId) {
        let (provider_swarm, provider_peer_id, provider_addr) = setup_provider_swarm().await;

        tokio::spawn(run_provider(provider_swarm, RejectingProvider));

        let downloader_swarm = setup_downloader_and_connect(provider_addr).await;
        (downloader_swarm, provider_peer_id)
    }

    async fn start_provider_with_stream_error() -> (Swarm<Behaviour>, PeerId) {
        let (provider_swarm, provider_peer_id, provider_addr) = setup_provider_swarm().await;

        tokio::spawn(run_provider(
            provider_swarm,
            ErrorStreamProvider { success_count: 1 },
        ));

        let downloader_swarm = setup_downloader_and_connect(provider_addr).await;
        (downloader_swarm, provider_peer_id)
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

    fn new_swarm_with_quic() -> Swarm<Behaviour> {
        let config = Config {
            peer_response_timeout: Duration::from_millis(1000),
        };
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
