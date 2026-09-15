use core::{
    num::{NonZeroU64, NonZeroUsize},
    task::{Context, Poll, Waker},
    time::Duration,
};
use std::{collections::VecDeque, io};

use futures::{FutureExt as _, future::BoxFuture};
use lb_blend_primitives::time::{Round, RoundClock, RoundCount};
use lb_log_targets::blend;
use libp2p::{
    PeerId, Stream, StreamProtocol,
    core::upgrade::ReadyUpgrade,
    swarm::{
        ConnectionHandlerEvent, ConnectionId, SubstreamProtocol,
        handler::{ConnectionEvent, FullyNegotiatedInbound, FullyNegotiatedOutbound},
    },
};

use crate::{
    FramingViolationError, OutgoingMessage,
    core::{admission::RoundShare, with_core::behaviour::handler::admission::SendQueue},
    flush_and_close_stream,
    message::IncomingMessage,
    recv_msg, send_msg,
};

pub mod admission;

const LOG_TARGET: &str = blend::network::core::core::conn::HANDLER;

pub struct ConnectionHandler {
    inbound_substream: Option<InboundSubstreamState>,
    outbound_substream: Option<OutboundSubstreamState>,
    send_queue: SendQueue,
    /// `r₁`: what this connection may still be read for this round.
    read_share: RoundShare,
    round_clock: RoundClock,
    pending_events_to_behaviour: VecDeque<ToBehaviour>,
    protocol_name: StreamProtocol,
    waker: Option<Waker>,
    connection_details: (PeerId, ConnectionId),
    /// Whether the behaviour has already been notified of a successful upgrade
    /// for this connection. Both inbound and outbound substreams must be
    /// negotiated, but the behaviour only needs to hear about it once. Once
    /// set, it stays set for the lifetime of the handler so that after
    /// [`Self::close_substreams`], a late-arriving upgrade event does not
    /// cause a second notification.
    upgrade_notified: bool,
    /// How many bytes one message occupies on this connection, which is fixed
    /// by the number of encapsulation layers and so the same for every message.
    message_size: NonZeroUsize,
    /// How long libp2p gives a substream upgrade before giving up on it.
    ///
    /// Sits outside `T_H` on purpose: the behaviour's own sweep is what should
    /// abandon a stalled handshake, because it is the behaviour that holds the
    /// degree slot and that can say why the handshake was given up on. This is
    /// the outer bound for when that does not happen, and it is derived from
    /// `T_H` rather than left at libp2p's defaults, which is tied to
    /// nothing in this protocol and would fire instead if we were to increase
    /// `T_H` above 10 seconds.
    upgrade_timeout: Duration,
}

type MsgSendFuture = BoxFuture<'static, Result<Stream, io::Error>>;
type MsgRecvFuture =
    BoxFuture<'static, Result<Option<(Stream, IncomingMessage)>, FramingViolationError>>;
type StreamCloseFuture = BoxFuture<'static, ()>;

enum InboundSubstreamState {
    /// The substream is open with no frame in flight.
    ///
    /// This is the only state in which reading may be suspended, and so the
    /// only safe place to stop.
    Idle(Stream),
    /// A frame is being received on the inbound substream.
    Receiving(MsgRecvFuture),
    /// A substream has been dropped proactively.
    Dropped,
}

enum OutboundSubstreamState {
    /// A request to open a new outbound substream is being processed.
    PendingOpenSubstream,
    /// An outbound substream is open and ready to send messages.
    Idle(Stream),
    /// A message is being sent on the outbound substream.
    PendingSend(MsgSendFuture),
    /// The connection is being closed, but a message is already part way onto
    /// the wire.
    ClosingAfterSend(MsgSendFuture),
    /// The stream is being ended cleanly.
    Closing(StreamCloseFuture),
    /// A substream has been dropped proactively.
    Dropped,
}

impl ConnectionHandler {
    pub fn new(
        protocol_name: StreamProtocol,
        connection_details: (PeerId, ConnectionId),
        round_clock: RoundClock,
        share_per_round: NonZeroU64,
        send_deadline: RoundCount,
        message_size: NonZeroUsize,
        upgrade_timeout: Duration,
    ) -> Self {
        tracing::trace!(target: LOG_TARGET, "Initializing core->core connection handler for connection {connection_details:?}.");
        let current_round = round_clock.current_round();
        Self {
            inbound_substream: None,
            outbound_substream: None,
            send_queue: SendQueue::new(
                RoundShare::new(share_per_round, current_round),
                send_deadline,
            ),
            read_share: RoundShare::new(share_per_round, current_round),
            round_clock,
            pending_events_to_behaviour: VecDeque::new(),
            protocol_name,
            waker: None,
            connection_details,
            upgrade_notified: false,
            message_size,
            upgrade_timeout,
        }
    }

    /// Refreshes both shares for the round now in progress, and gives up on
    /// whatever has waited too long to be sent.
    fn process_current_round(&mut self, cx: &mut Context<'_>) -> Round {
        let current_round = self.round_clock.poll_current(cx);
        self.read_share.refill_for(current_round);
        let discarded = self.send_queue.enter_round(current_round);
        if discarded > 0 {
            tracing::debug!(
                target: LOG_TARGET,
                "Gave up on {discarded} message(s) waiting to be sent on connection {:?}: they waited longer than a message may spend at one hop. Copies queued for other neighbours are unaffected.",
                self.connection_details
            );
        }
        current_round
    }

    /// Emit a [`ToBehaviour::FullyNegotiated`] event if one has not already
    /// been emitted for this connection. Both inbound and outbound substreams
    /// need to be negotiated before the connection is usable, but the
    /// behaviour only needs to hear about the upgrade once, so we dedupe here.
    fn check_and_notify_about_upgrade(&mut self) {
        if !self.upgrade_notified {
            self.pending_events_to_behaviour
                .push_back(ToBehaviour::FullyNegotiated);
            self.upgrade_notified = true;
        }
    }

    /// Mark the inbound/outbound substream state as Dropped.
    /// Then the substream hold by the state will be dropped from memory.
    /// As a result, Swarm will decrease the ref count to the connection,
    /// and close the connection when the count is 0.
    ///
    /// Also, this clears all pending messages and events
    /// to avoid confusions for event recipients.
    /// Closes both substreams, letting a message already part way onto the wire
    /// finish first.
    fn close_substreams(&mut self) {
        self.inbound_substream = Some(InboundSubstreamState::Dropped);
        self.outbound_substream = Some(match self.outbound_substream.take() {
            Some(OutboundSubstreamState::PendingSend(sending)) => {
                OutboundSubstreamState::ClosingAfterSend(sending)
            }
            Some(OutboundSubstreamState::Idle(stream)) => {
                OutboundSubstreamState::Closing(flush_and_close_stream(stream).boxed())
            }
            Some(
                state @ (OutboundSubstreamState::ClosingAfterSend(_)
                | OutboundSubstreamState::Closing(_)),
            ) => state,
            _ => OutboundSubstreamState::Dropped,
        });
        // Messages that never reached the wire are simply dropped: nothing is
        // owed to a neighbour for a message it has seen no byte of.
        self.send_queue.clear();
        self.pending_events_to_behaviour.clear();
    }

    fn try_wake(&mut self) {
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }
}

#[derive(Debug)]
pub enum FromBehaviour {
    /// A message to be sent to the connection.
    Message(OutgoingMessage),
    /// Close inbound/outbound substreams.
    /// This happens when [`crate::Behaviour`] determines that one of the
    /// followings is true.
    /// - Max peering degree is reached.
    /// - The peer has been detected as malicious.
    CloseSubstreams,
}

#[derive(Debug)]
pub enum ToBehaviour {
    /// The connection has been successfully upgraded for the blend protocol.
    /// Emitted at most once per connection, on the first successful upgrade
    /// of either the inbound or outbound substream.
    FullyNegotiated,
    /// A message has been received from the connection.
    Message(IncomingMessage),
    /// The inbound stream broke the connection's framing. Attributable to the
    /// neighbour: the transport authenticates every byte it carries, and this
    /// node never leaves a message half-written, not even while closing.
    /// The inbound/outbound streams to the peer are closed proactively.
    InboundFramingViolation(io::Error),
    /// An IO error from the connection that is nobody's fault: this node's own
    /// send failed.
    /// The inbound/outbound streams to the peer are closed proactively.
    IOError(io::Error),
}

impl libp2p::swarm::ConnectionHandler for ConnectionHandler {
    type FromBehaviour = FromBehaviour;
    type ToBehaviour = ToBehaviour;
    type InboundProtocol = ReadyUpgrade<StreamProtocol>;
    type InboundOpenInfo = ();
    type OutboundProtocol = ReadyUpgrade<StreamProtocol>;
    type OutboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        SubstreamProtocol::new(ReadyUpgrade::new(self.protocol_name.clone()), ())
            .with_timeout(self.upgrade_timeout)
    }

    #[expect(clippy::too_many_lines, reason = "TODO: Address this at some point.")]
    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: address this in a dedicated refactor"
    )]
    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>,
    > {
        // Nothing left to do once both substreams are gone. The outbound one
        // may still be finishing a message and ending the stream cleanly even
        // though the inbound one is already gone, and that work is exactly what
        // keeps a neighbour from reading this node's close as a fault, so it
        // has to keep being polled.
        if matches!(self.inbound_substream, Some(InboundSubstreamState::Dropped))
            && matches!(
                self.outbound_substream,
                Some(OutboundSubstreamState::Dropped)
            )
        {
            return Poll::Pending;
        }

        self.process_current_round(cx);

        // Process pending events to be sent to the behaviour
        if let Some(event) = self.pending_events_to_behaviour.pop_front() {
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
        }

        // Process inbound stream
        loop {
            match self.inbound_substream.take() {
                None => break,
                Some(InboundSubstreamState::Dropped) => {
                    self.inbound_substream = Some(InboundSubstreamState::Dropped);
                    break;
                }
                Some(InboundSubstreamState::Idle(stream)) => {
                    if self.read_share.try_spend() {
                        self.inbound_substream = Some(InboundSubstreamState::Receiving(
                            recv_msg(stream, self.message_size).boxed(),
                        ));
                        continue;
                    }
                    // The share for this round is spent, so no read is issued.
                    // The bytes stay in the transport, where the flow control
                    // of the connection pushes back on the neighbour, and the
                    // clock polled above will wake us when the share refreshes.
                    tracing::trace!(
                        target: LOG_TARGET,
                        "Read share for connection {:?} is spent; not reading again until the next round.",
                        self.connection_details
                    );
                    self.inbound_substream = Some(InboundSubstreamState::Idle(stream));
                    break;
                }
                Some(InboundSubstreamState::Receiving(mut msg_recv_fut)) => {
                    match msg_recv_fut.poll_unpin(cx) {
                        Poll::Ready(Ok(Some((stream, msg)))) => {
                            tracing::trace!(
                                target: LOG_TARGET,
                                "Received message from inbound stream {:?}; notifying behaviour",
                                self.connection_details
                            );
                            self.inbound_substream = Some(InboundSubstreamState::Idle(stream));
                            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                                ToBehaviour::Message(msg),
                            ));
                        }
                        // The neighbour ended the stream between messages,
                        // which is how a connection ends when the protocol asks
                        // for it.
                        Poll::Ready(Ok(None)) => {
                            tracing::trace!(
                                target: LOG_TARGET,
                                "Peer closed inbound stream {:?} between messages. Dropping both inbound/outbound substreams",
                                self.connection_details
                            );
                            self.close_substreams();
                        }
                        // A message that stops part way through. This node
                        // finishes what it has already started before closing,
                        // so the only way a neighbour is left holding half a
                        // message is if the neighbour left it that way.
                        Poll::Ready(Err(FramingViolationError(error))) => {
                            tracing::debug!(target: LOG_TARGET, "Inbound stream {:?} broke the connection's framing: {error}. Dropping both inbound/outbound substreams", self.connection_details);
                            self.close_substreams();
                            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                                ToBehaviour::InboundFramingViolation(error),
                            ));
                        }
                        Poll::Pending => {
                            self.inbound_substream =
                                Some(InboundSubstreamState::Receiving(msg_recv_fut));
                            break;
                        }
                    }
                }
            }
        }

        // Process outbound stream
        // TODO: Refactor this to a separate function.
        loop {
            match self.outbound_substream.take() {
                // If the request to open a new outbound substream is still being processed, wait
                // more.
                Some(OutboundSubstreamState::PendingOpenSubstream) => {
                    self.outbound_substream = Some(OutboundSubstreamState::PendingOpenSubstream);
                    self.waker = Some(cx.waker().clone());
                    return Poll::Pending;
                }
                // If the substream is idle, and if it's time to send a message, send it.
                Some(OutboundSubstreamState::Idle(stream)) => {
                    if let Some(msg) = self.send_queue.pop_front() {
                        tracing::trace!(target: LOG_TARGET, "Sending message to outbound stream {:?}", self.connection_details);
                        self.outbound_substream = Some(OutboundSubstreamState::PendingSend(
                            send_msg(stream, msg).boxed(),
                        ));
                    } else {
                        self.outbound_substream = Some(OutboundSubstreamState::Idle(stream));
                        self.waker = Some(cx.waker().clone());
                        return Poll::Pending;
                    }
                }
                // If a message is being sent, check if it's done.
                Some(OutboundSubstreamState::PendingSend(mut msg_send_fut)) => {
                    match msg_send_fut.poll_unpin(cx) {
                        Poll::Ready(Ok(stream)) => {
                            tracing::trace!(target: LOG_TARGET, "Message sent to outbound stream {:?}", self.connection_details);
                            self.outbound_substream = Some(OutboundSubstreamState::Idle(stream));
                        }
                        Poll::Ready(Err(e)) => {
                            tracing::error!(target: LOG_TARGET, "Failed to send message to outbound stream {:?}: {e:?}. Dropping both inbound and outbound substreams", self.connection_details);
                            self.close_substreams();
                            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                                ToBehaviour::IOError(e),
                            ));
                        }
                        Poll::Pending => {
                            self.outbound_substream =
                                Some(OutboundSubstreamState::PendingSend(msg_send_fut));
                            self.waker = Some(cx.waker().clone());
                            return Poll::Pending;
                        }
                    }
                }
                // Finishing the message already on the wire, and then letting
                // the substream go. Whether the send succeeds no longer
                // matters — the connection is closing either way — only that
                // the neighbour is not left holding part of a message.
                Some(OutboundSubstreamState::ClosingAfterSend(mut sending)) => {
                    match sending.poll_unpin(cx) {
                        Poll::Ready(Ok(stream)) => {
                            tracing::trace!(target: LOG_TARGET, "Finished the message in flight on outbound stream {:?}; ending the stream", self.connection_details);
                            self.outbound_substream = Some(OutboundSubstreamState::Closing(
                                flush_and_close_stream(stream).boxed(),
                            ));
                        }
                        // The send failed, so there is no stream left to end
                        // cleanly and nothing further this node can do for the
                        // neighbour on the other side of it.
                        Poll::Ready(Err(e)) => {
                            tracing::debug!(target: LOG_TARGET, "The message in flight on outbound stream {:?} could not be finished: {e}", self.connection_details);
                            self.outbound_substream = Some(OutboundSubstreamState::Dropped);
                        }
                        Poll::Pending => {
                            self.outbound_substream =
                                Some(OutboundSubstreamState::ClosingAfterSend(sending));
                            self.waker = Some(cx.waker().clone());
                            return Poll::Pending;
                        }
                    }
                }
                Some(OutboundSubstreamState::Closing(mut closing)) => {
                    match closing.poll_unpin(cx) {
                        Poll::Ready(()) => {
                            tracing::trace!(target: LOG_TARGET, "Ended outbound stream {:?} cleanly", self.connection_details);
                            self.outbound_substream = Some(OutboundSubstreamState::Dropped);
                        }
                        Poll::Pending => {
                            self.outbound_substream =
                                Some(OutboundSubstreamState::Closing(closing));
                            self.waker = Some(cx.waker().clone());
                            return Poll::Pending;
                        }
                    }
                }
                Some(OutboundSubstreamState::Dropped) => {
                    tracing::trace!(target: LOG_TARGET, "Outbound substream {:?} dropped proactively", self.connection_details);
                    self.outbound_substream = Some(OutboundSubstreamState::Dropped);
                    return Poll::Pending;
                }
                // If there is no outbound substream, request to open a new one.
                None => {
                    tracing::trace!(
                        target: LOG_TARGET,
                        "Outbound substream {:?} not initialized yet; requesting swarm to open one", self.connection_details
                    );
                    self.outbound_substream = Some(OutboundSubstreamState::PendingOpenSubstream);
                    return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                        protocol: SubstreamProtocol::new(
                            ReadyUpgrade::new(self.protocol_name.clone()),
                            (),
                        )
                        .with_timeout(self.upgrade_timeout),
                    });
                }
            }
        }
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        match event {
            FromBehaviour::Message(msg) => {
                // The deadline runs from the round the message joined this
                // connection's queue, so it is set here rather than when the
                // message reaches the front.
                self.send_queue
                    .enqueue(msg, self.round_clock.current_round());
            }
            FromBehaviour::CloseSubstreams => {
                self.close_substreams();
            }
        }
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: address this in a dedicated refactor"
    )]
    fn on_connection_event(
        &mut self,
        event: ConnectionEvent<
            Self::InboundProtocol,
            Self::OutboundProtocol,
            Self::InboundOpenInfo,
            Self::OutboundOpenInfo,
        >,
    ) {
        match event {
            ConnectionEvent::FullyNegotiatedInbound(FullyNegotiatedInbound {
                protocol: stream,
                ..
            }) => {
                // If `close_substreams` has already run, the behaviour considers
                // this connection closed. Overwriting the Dropped state with an
                // open stream here would resurrect the substream and keep the
                // connection alive from libp2p's perspective, even though the
                // behaviour has stopped tracking it.
                if matches!(self.inbound_substream, Some(InboundSubstreamState::Dropped)) {
                    tracing::debug!(target: LOG_TARGET, "Dropping late inbound upgrade for already-closed connection {:?}.", self.connection_details);
                    drop(stream);
                } else {
                    tracing::trace!(target: LOG_TARGET, "Fully negotiated inbound for connection {:?}; creating inbound substream", self.connection_details);
                    self.inbound_substream = Some(InboundSubstreamState::Idle(stream));
                    self.check_and_notify_about_upgrade();
                }
            }
            ConnectionEvent::FullyNegotiatedOutbound(FullyNegotiatedOutbound {
                protocol: stream,
                ..
            }) => {
                if matches!(
                    self.outbound_substream,
                    Some(OutboundSubstreamState::Dropped)
                ) {
                    tracing::debug!(target: LOG_TARGET, "Dropping late outbound upgrade for already-closed connection {:?}.", self.connection_details);
                    drop(stream);
                } else {
                    tracing::trace!(target: LOG_TARGET, "Fully negotiated outbound for connection {:?}; creating outbound substream", self.connection_details);
                    self.outbound_substream = Some(OutboundSubstreamState::Idle(stream));
                    self.check_and_notify_about_upgrade();
                }
            }
            ConnectionEvent::DialUpgradeError(e) => {
                // This error is handled in the swarm, so we just log it at `DEBUG` level here.
                tracing::debug!(target: LOG_TARGET, "DialUpgradeError for connection {:?}: {:?}", self.connection_details, e);
                self.close_substreams();
            }
            event => {
                tracing::trace!(target: LOG_TARGET, ?event, "Ignoring connection event");
            }
        }

        self.try_wake();
    }
}
