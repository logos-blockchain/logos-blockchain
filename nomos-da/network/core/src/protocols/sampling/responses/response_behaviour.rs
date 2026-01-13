use std::{
    collections::VecDeque,
    task::{Context, Poll, Waker},
};

use futures::{
    AsyncWriteExt as _, FutureExt as _, StreamExt as _,
    channel::{
        oneshot,
        oneshot::{Receiver, Sender},
    },
    stream::FuturesUnordered,
};
use libp2p::{
    Multiaddr, PeerId,
    core::{Endpoint, transport::PortUse},
    swarm::{
        ConnectionDenied, ConnectionId, DialFailure, FromSwarm, NetworkBehaviour, THandler,
        THandlerInEvent, THandlerOutEvent, ToSwarm,
    },
};
use libp2p_stream::IncomingStreams;
use nomos_da_messages::sampling;

use crate::{
    protocol::SAMPLING_PROTOCOL,
    protocols::sampling::{
        BehaviourSampleReq, BehaviourSampleRes, ResponseChannel, SamplingRequestStreamFuture,
        SubnetsConfig,
        connections::Connections,
        errors::SamplingError,
        responses::SamplingEvent,
        streams::{self, SampleStream},
    },
};

/// Executor sampling protocol
/// Takes care of sending and replying sampling requests
pub struct ResponseSamplingBehaviour {
    /// Underlying stream behaviour
    stream_behaviour: libp2p_stream::Behaviour,
    /// Incoming sample request streams
    incoming_streams: IncomingStreams,
    /// Pending sampling stream tasks
    stream_tasks: FuturesUnordered<SamplingRequestStreamFuture>,
    /// Sample streams that has no tasks and should be closed.
    to_close: VecDeque<SampleStream>,
    /// Pending subnetwork connections for randomly selected peers after refresh
    /// interval.
    connections: Connections,
    /// Waker for sampling polling
    waker: Option<Waker>,
}

impl ResponseSamplingBehaviour {
    pub fn new(subnets_config: SubnetsConfig) -> Self {
        let stream_behaviour = libp2p_stream::Behaviour::new();
        let mut control = stream_behaviour.new_control();

        let incoming_streams = control
            .accept(SAMPLING_PROTOCOL)
            .expect("Just a single accept to protocol is valid");

        let stream_tasks = FuturesUnordered::new();
        let to_close = VecDeque::new();

        let connections = Connections::new(subnets_config.shares_retry_limit);

        Self {
            stream_behaviour,
            incoming_streams,
            stream_tasks,
            to_close,
            connections,
            waker: None,
        }
    }

    /// Schedule an incoming stream to be replied
    /// Creates the necessary channels so requests can be replied from outside
    /// of this behaviour from whoever that takes the channels
    fn schedule_incoming_stream_task(
        &self,
        sample_stream: SampleStream,
    ) -> (Receiver<BehaviourSampleReq>, Sender<BehaviourSampleRes>) {
        let (request_sender, request_receiver) = oneshot::channel();
        let (response_sender, response_receiver) = oneshot::channel();
        let channel = ResponseChannel {
            request_sender,
            response_receiver,
        };
        self.stream_tasks
            .push(streams::handle_incoming_stream(sample_stream, channel).boxed());
        // Scheduled a task, lets poll again.
        (request_receiver, response_sender)
    }

    pub fn try_wake(&mut self) {
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }
    fn handle_stream_response(
        &self,
        _peer_id: PeerId,
        stream: SampleStream,
    ) -> Poll<ToSwarm<<Self as NetworkBehaviour>::ToSwarm, THandlerInEvent<Self>>> {
        // Writer might behoping to send to this stream another request, wait
        // until the writer closes the stream.
        let (request_receiver, response_sender) = self.schedule_incoming_stream_task(stream);
        Poll::Ready(ToSwarm::GenerateEvent(SamplingEvent::IncomingSample {
            request_receiver,
            response_sender,
        }))
    }

    fn handle_stream_error(
        &mut self,
        error: SamplingError,
        maybe_stream: Option<SampleStream>,
    ) -> Option<Poll<ToSwarm<<Self as NetworkBehaviour>::ToSwarm, THandlerInEvent<Self>>>> {
        match error {
            SamplingError::Io {
                error,
                message: Some(sampling::SampleRequest::Share(message)),
                ..
            } if error.kind() == std::io::ErrorKind::ConnectionReset => {
                // Propagate error for blob_id if retry limit is reached.
                if !self.connections.should_retry(message.share_idx) {
                    return Some(Poll::Ready(ToSwarm::GenerateEvent(
                        SamplingEvent::SamplingError {
                            error: SamplingError::NoSubnetworkPeers {
                                blob_id: message.blob_id,
                                subnetwork_id: message.share_idx,
                            },
                        },
                    )));
                }
                // Stream is useless if connection was reset.
                if let Some(stream) = maybe_stream {
                    self.to_close.push_back(stream);
                }
                None
            }
            SamplingError::Io { error, .. }
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                // Eof is actually expected and is proper signal about remote closing the
                // stream. Do not propagate and continue execution of behaviour poll method.
                if let Some(stream) = maybe_stream {
                    self.to_close.push_back(stream);
                }
                None
            }
            error => {
                if let Some(stream) = maybe_stream {
                    self.to_close.push_back(stream);
                }
                Some(Poll::Ready(ToSwarm::GenerateEvent(
                    SamplingEvent::SamplingError { error },
                )))
            }
        }
    }

    fn poll_stream_tasks(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Option<Poll<ToSwarm<<Self as NetworkBehaviour>::ToSwarm, THandlerInEvent<Self>>>> {
        if let Poll::Ready(Some(future_result)) = self.stream_tasks.poll_next_unpin(cx) {
            cx.waker().wake_by_ref();
            match future_result {
                Ok((peer_id, stream)) => {
                    return Some(self.handle_stream_response(peer_id, stream));
                }
                Err((error, maybe_stream)) => {
                    return self.handle_stream_error(error, maybe_stream);
                }
            }
        }
        None
    }
}

impl NetworkBehaviour for ResponseSamplingBehaviour {
    type ConnectionHandler = <libp2p_stream::Behaviour as NetworkBehaviour>::ConnectionHandler;
    type ToSwarm = SamplingEvent;

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
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
        self.connections.register_connect(peer);
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
        if let FromSwarm::DialFailure(DialFailure {
            peer_id: Some(peer_id),
            ..
        }) = event
        {
            self.connections.register_disconnect(peer_id);
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
        self.waker = Some(cx.waker().clone());
        // poll incoming streams
        if let Poll::Ready(Some((peer_id, stream))) = self.incoming_streams.poll_next_unpin(cx) {
            let sample_stream = SampleStream { stream, peer_id };
            let (request_receiver, response_sender) =
                self.schedule_incoming_stream_task(sample_stream);
            cx.waker().wake_by_ref();
            return Poll::Ready(ToSwarm::GenerateEvent(SamplingEvent::IncomingSample {
                request_receiver,
                response_sender,
            }));
        }

        // poll stream tasks
        if let Some(result) = self.poll_stream_tasks(cx) {
            return result;
        }

        // Discard stream, if still pending pushback to close later.
        if let Some(mut stream) = self.to_close.pop_front()
            && stream.stream.close().poll_unpin(cx).is_pending()
        {
            self.to_close.push_back(stream);
            cx.waker().wake_by_ref();
        }

        Poll::Pending
    }
}
