use core::{
    ops::RangeInclusive,
    task::{Context, Poll, Waker},
};
use std::{collections::VecDeque, io};

use futures::{FutureExt as _, future::BoxFuture};
use lb_log_targets::blend;
use libp2p::{
    PeerId, Stream, StreamProtocol,
    core::upgrade::ReadyUpgrade,
    swarm::{
        ConnectionHandlerEvent, ConnectionId, SubstreamProtocol,
        handler::{
            ConnectionEvent, DialUpgradeError, FullyNegotiatedInbound, FullyNegotiatedOutbound,
        },
    },
};

use crate::{
    core::with_core::behaviour::handler::conn_maintenance::{
        ConnectionMonitor, ConnectionMonitorOutput,
    },
    recv_msg, send_msg,
};

pub(super) mod conn_maintenance;

const LOG_TARGET: &str = blend::network::core::core::conn::HANDLER;

pub struct ConnectionHandler<ConnectionWindowClock> {
    inbound_substream: Option<InboundSubstreamState>,
    outbound_substream: Option<OutboundSubstreamState>,
    outbound_msgs: VecDeque<Vec<u8>>,
    pending_events_to_behaviour: VecDeque<ToBehaviour>,
    monitor: ConnectionMonitor<ConnectionWindowClock>,
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
}

type MsgSendFuture = BoxFuture<'static, Result<Stream, io::Error>>;
type MsgRecvFuture = BoxFuture<'static, Result<(Stream, Vec<u8>), io::Error>>;

enum InboundSubstreamState {
    /// A message is being received on the inbound substream.
    PendingRecv(MsgRecvFuture),
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
    /// A substream has been dropped proactively.
    Dropped,
}

impl<ConnectionWindowClock> ConnectionHandler<ConnectionWindowClock> {
    pub fn new(
        monitor: ConnectionMonitor<ConnectionWindowClock>,
        protocol_name: StreamProtocol,
        connection_details: (PeerId, ConnectionId),
    ) -> Self {
        tracing::trace!(target: LOG_TARGET, "Initializing core->core connection handler for connection {connection_details:?}.");
        Self {
            inbound_substream: None,
            outbound_substream: None,
            outbound_msgs: VecDeque::new(),
            pending_events_to_behaviour: VecDeque::new(),
            monitor,
            protocol_name,
            waker: None,
            connection_details,
            upgrade_notified: false,
        }
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
    fn close_substreams(&mut self) {
        self.inbound_substream = Some(InboundSubstreamState::Dropped);
        self.outbound_substream = Some(OutboundSubstreamState::Dropped);
        self.outbound_msgs.clear();
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
    Message(Vec<u8>),
    /// Close inbound/outbound substreams.
    /// This happens when [`crate::Behaviour`] determines that one of the
    /// followings is true.
    /// - Max peering degree is reached.
    /// - The peer has been detected as spammy.
    CloseSubstreams,
}

#[derive(Debug)]
pub enum ToBehaviour {
    /// The connection has been successfully upgraded for the blend protocol.
    /// Emitted at most once per connection, on the first successful upgrade
    /// of either the inbound or outbound substream.
    FullyNegotiated,
    /// An outbound substream was failed to be upgraded for the blend protocol.
    DialUpgradeError(DialUpgradeError<(), ReadyUpgrade<StreamProtocol>>),
    /// A message has been received from the connection.
    Message(Vec<u8>),
    /// Notifying that the peer is detected as spammy.
    /// The inbound/outbound streams to the peer are closed proactively.
    SpammyPeer,
    /// Notifying that the peer is detected as unhealthy.
    UnhealthyPeer,
    /// Notifying that the peer is detected as healthy.
    HealthyPeer,
    /// An IO error from the connection.
    /// The inbound/outbound streams to the peer are closed proactively.
    IOError(io::Error),
}

impl<ConnectionWindowClock> libp2p::swarm::ConnectionHandler
    for ConnectionHandler<ConnectionWindowClock>
where
    ConnectionWindowClock: futures::Stream<Item = RangeInclusive<u64>> + Unpin + Send + 'static,
{
    type FromBehaviour = FromBehaviour;
    type ToBehaviour = ToBehaviour;
    type InboundProtocol = ReadyUpgrade<StreamProtocol>;
    type InboundOpenInfo = ();
    type OutboundProtocol = ReadyUpgrade<StreamProtocol>;
    type OutboundOpenInfo = ();

    #[expect(deprecated, reason = "Self::InboundOpenInfo is deprecated")]
    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        SubstreamProtocol::new(ReadyUpgrade::new(self.protocol_name.clone()), ())
    }

    #[expect(deprecated, reason = "Self::OutboundOpenInfo is deprecated")]
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
        // Short-circuit so that we do not poll the connection monitor anymore in case
        // either of the two substreams has been dropped.
        if matches!(self.inbound_substream, Some(InboundSubstreamState::Dropped))
            || matches!(
                self.outbound_substream,
                Some(OutboundSubstreamState::Dropped)
            )
        {
            return Poll::Pending;
        }

        // Check if the monitor interval has elapsed, if exists.
        // TODO: Refactor this to a separate function.
        if let Poll::Ready(output) = self.monitor.poll(cx) {
            match output {
                Some(ConnectionMonitorOutput::Spammy) => {
                    // TODO: Re-enable this once we have fixed Blend observation
                    // window range values.
                    // self.close_substreams();
                    self.pending_events_to_behaviour
                        .push_back(ToBehaviour::SpammyPeer);
                }
                Some(ConnectionMonitorOutput::Unhealthy) => {
                    self.pending_events_to_behaviour
                        .push_back(ToBehaviour::UnhealthyPeer);
                }
                Some(ConnectionMonitorOutput::Healthy) => {
                    self.pending_events_to_behaviour
                        .push_back(ToBehaviour::HealthyPeer);
                }
                None => {
                    tracing::debug!(
                        target: LOG_TARGET,
                        "Connection monitor for connection {:?} closed unexpectedly. Closing substreams proactively.",
                        self.connection_details
                    );
                    self.close_substreams();
                }
            }
        }

        // Process pending events to be sent to the behaviour
        if let Some(event) = self.pending_events_to_behaviour.pop_front() {
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
        }

        // Process inbound stream
        // TODO: Refactor this to a separate function.
        match self.inbound_substream.take() {
            None => {}
            Some(InboundSubstreamState::PendingRecv(mut msg_recv_fut)) => match msg_recv_fut
                .poll_unpin(cx)
            {
                Poll::Ready(Ok((stream, msg))) => {
                    tracing::trace!(
                        target: LOG_TARGET,
                        "Received message from inbound stream {:?}; notifying behaviour",
                        self.connection_details
                    );

                    // Record the message to the monitor.
                    self.monitor.record_message();

                    self.inbound_substream =
                        Some(InboundSubstreamState::PendingRecv(recv_msg(stream).boxed()));

                    // Notify behaviour.
                    return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                        ToBehaviour::Message(msg),
                    ));
                }
                Poll::Ready(Err(e)) => {
                    tracing::error!(target: LOG_TARGET, "Failed to receive message from inbound stream {:?}: {e:?}. Dropping both inbound/outbound substreams", self.connection_details);
                    self.close_substreams();
                    return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                        ToBehaviour::IOError(e),
                    ));
                }
                Poll::Pending => {
                    self.inbound_substream = Some(InboundSubstreamState::PendingRecv(msg_recv_fut));
                }
            },
            Some(InboundSubstreamState::Dropped) => {
                self.inbound_substream = Some(InboundSubstreamState::Dropped);
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
                    if let Some(msg) = self.outbound_msgs.pop_front() {
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
                        ),
                    });
                }
            }
        }
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        match event {
            FromBehaviour::Message(msg) => {
                self.outbound_msgs.push_back(msg);
            }
            FromBehaviour::CloseSubstreams => {
                self.close_substreams();
            }
        }
    }

    #[expect(
        deprecated,
        reason = "Self::InboundOpenInfo and Self::OutboundOpenInfo are deprecated"
    )]
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
                    self.inbound_substream =
                        Some(InboundSubstreamState::PendingRecv(recv_msg(stream).boxed()));
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
                tracing::error!(target: LOG_TARGET, "DialUpgradeError for connection {:?}: {:?}", self.connection_details, e);
                self.pending_events_to_behaviour
                    .push_back(ToBehaviour::DialUpgradeError(e));
                self.close_substreams();
            }
            event => {
                tracing::trace!(target: LOG_TARGET, ?event, "Ignoring connection event");
            }
        }

        self.try_wake();
    }
}
