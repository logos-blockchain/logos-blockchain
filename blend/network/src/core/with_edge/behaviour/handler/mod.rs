use core::{
    pin::Pin,
    task::{Context, Poll, Waker},
    time::Duration,
};
use std::io;

use lb_log_targets::blend;
use libp2p::{
    StreamProtocol,
    core::upgrade::{DeniedUpgrade, ReadyUpgrade},
    swarm::{ConnectionHandlerEvent, SubstreamProtocol},
};

use crate::core::with_edge::behaviour::handler::{
    dropped::DroppedState, ready_to_receive::ReadyToReceiveState, receiving::ReceivingState,
    starting::StartingState,
};

mod dropped;
mod ready_to_receive;
mod receiving;
mod starting;

const LOG_TARGET: &str = blend::network::core::handler::CORE_EDGE;

type TimerFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
type MessageReceiveFuture = Pin<Box<dyn Future<Output = Result<Vec<u8>, io::Error>> + Send>>;
#[expect(deprecated, reason = "Self::InboundOpenInfo is deprecated")]
type PollResult<T> = (
    Poll<
        ConnectionHandlerEvent<
            <ConnectionHandler as libp2p::swarm::ConnectionHandler>::OutboundProtocol,
            <ConnectionHandler as libp2p::swarm::ConnectionHandler>::OutboundOpenInfo,
            ToBehaviour,
        >,
    >,
    T,
);
#[expect(deprecated, reason = "Self::InboundOpenInfo is deprecated")]
type ConnectionEvent<'a> = libp2p::swarm::handler::ConnectionEvent<
    'a,
    <ConnectionHandler as libp2p::swarm::ConnectionHandler>::InboundProtocol,
    <ConnectionHandler as libp2p::swarm::ConnectionHandler>::OutboundProtocol,
    <ConnectionHandler as libp2p::swarm::ConnectionHandler>::InboundOpenInfo,
    <ConnectionHandler as libp2p::swarm::ConnectionHandler>::OutboundOpenInfo,
>;

pub enum ConnectionState {
    Starting(StartingState),
    ReadyToReceive(ReadyToReceiveState),
    Receiving(ReceivingState),
    Dropped(DroppedState),
}

impl ConnectionState {
    fn on_behaviour_event(self, event: FromBehaviour) -> Self {
        match self {
            Self::Starting(s) => s.on_behaviour_event(event),
            Self::ReadyToReceive(s) => s.on_behaviour_event(event),
            Self::Receiving(s) => s.on_behaviour_event(event),
            Self::Dropped(s) => s.on_behaviour_event(event),
        }
    }

    fn on_connection_event(self, event: ConnectionEvent) -> Self {
        match self {
            Self::Starting(s) => s.on_connection_event(event),
            Self::ReadyToReceive(s) => s.on_connection_event(event),
            Self::Receiving(s) => s.on_connection_event(event),
            Self::Dropped(s) => s.on_connection_event(event),
        }
    }

    fn poll(self, cx: &mut Context<'_>) -> PollResult<Self> {
        match self {
            Self::Starting(s) => s.poll(cx),
            Self::ReadyToReceive(s) => s.poll(cx),
            Self::Receiving(s) => s.poll(cx),
            Self::Dropped(s) => s.poll(cx),
        }
    }
}

trait StateTrait: Into<ConnectionState> {
    fn on_behaviour_event(mut self, event: FromBehaviour) -> ConnectionState {
        if matches!(event, FromBehaviour::CloseSubstream) {
            return DroppedState::new(None, self.take_waker()).into();
        }
        tracing::trace!(target: LOG_TARGET, "Ignore behaviour event: {event:?}.");
        self.into()
    }

    fn on_connection_event(self, _event: ConnectionEvent) -> ConnectionState {
        self.into()
    }

    fn poll(self, cx: &mut Context<'_>) -> PollResult<ConnectionState>;

    fn take_waker(&mut self) -> Option<Waker>;
}

pub struct ConnectionHandler {
    state: Option<ConnectionState>,
    protocol_name: StreamProtocol,
}

impl ConnectionHandler {
    pub fn new(connection_timeout: Duration, protocol_name: StreamProtocol) -> Self {
        tracing::trace!(target: LOG_TARGET, "Initializing core->edge connection handler with timeout duration {connection_timeout:?}.");
        Self {
            state: Some(StartingState::new(connection_timeout).into()),
            protocol_name,
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FailureReason {
    Timeout,
    MessageStream,
    UpgradeError,
}

#[derive(Debug)]
pub enum FromBehaviour {
    CloseSubstream,
    StartReceiving,
}

#[derive(Debug)]
pub enum ToBehaviour {
    /// A message has been received from the connection.
    Message(Vec<u8>),
    SubstreamOpened,
    SubstreamClosed(Option<FailureReason>),
}

impl libp2p::swarm::ConnectionHandler for ConnectionHandler {
    type FromBehaviour = FromBehaviour;
    type ToBehaviour = ToBehaviour;
    type InboundProtocol = ReadyUpgrade<StreamProtocol>;
    type InboundOpenInfo = ();
    type OutboundProtocol = DeniedUpgrade;
    type OutboundOpenInfo = ();

    #[expect(deprecated, reason = "Self::InboundOpenInfo is deprecated")]
    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        SubstreamProtocol::new(ReadyUpgrade::new(self.protocol_name.clone()), ())
    }

    // We need this override because the Swarm is configured with a keepalive
    // timeout of `0`, which causes connections with edge nodes to be dropped before
    // there is even an active stream. So we manage the keepalive state ourselves,
    // by marking the connection as stale if the connection handler is dropped for
    // any reason.
    fn connection_keep_alive(&self) -> bool {
        !matches!(self.state, Some(ConnectionState::Dropped(_)))
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        let state = self.state.take().expect("Inconsistent state");
        self.state = Some(state.on_behaviour_event(event));
    }

    fn on_connection_event(&mut self, event: ConnectionEvent) {
        let state = self.state.take().expect("Inconsistent state");
        self.state = Some(state.on_connection_event(event));
    }

    #[expect(deprecated, reason = "Self::InboundOpenInfo is deprecated")]
    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>,
    > {
        let state = self.state.take().expect("Inconsistent state");

        let (poll_result, new_state) = state.poll(cx);
        self.state = Some(new_state);

        poll_result
    }
}

#[cfg(test)]
mod tests {
    use core::{
        task::{Context, Poll},
        time::Duration,
    };

    use futures::task::noop_waker_ref;
    use libp2p::{StreamProtocol, swarm::ConnectionHandler as _};

    use super::ConnectionHandler;

    /// Reproduces audit finding #9: an edge connection that never opens its
    /// inbound substream is never closed, leaking the connection.
    ///
    /// The handler starts in `Starting`, whose `poll` only stores the waker and
    /// returns `Pending` — it never arms a timer. The `connection_timeout` is
    /// consumed only when transitioning to `ReadyToReceive` on
    /// `FullyNegotiatedInbound`. Meanwhile `connection_keep_alive()` returns
    /// `true` for every state except `Dropped`, so the swarm keeps the
    /// connection open. A peer that establishes the connection but never opens
    /// the inbound substream therefore pins the handler in `Starting` forever.
    /// The behaviour's `max_incoming_connections` cap only counts *upgraded*
    /// peers (`upgraded_edge_peers`, populated on `SubstreamOpened`), so these
    /// stuck connections bypass it entirely — an unbounded, remote-triggered
    /// connection leak (FD/memory exhaustion).
    ///
    /// This is a characterization test: it asserts the *current, buggy*
    /// behavior (still keep-alive, still `Pending` long after the timeout
    /// has elapsed). When the bug is fixed (arm the timeout in `Starting`
    /// as well), `poll` will yield `SubstreamClosed(Timeout)` and
    /// `connection_keep_alive()` will drop — flipping both assertions,
    /// which should then be updated to assert the correct behavior.
    #[tokio::test]
    async fn starting_state_without_inbound_substream_leaks_connection() {
        // Short timeout so the test is fast. The state machine uses
        // `futures_timer::Delay` (real wall-clock), not tokio's mock clock.
        let connection_timeout = Duration::from_millis(50);
        let mut handler =
            ConnectionHandler::new(connection_timeout, StreamProtocol::new("/blend/edge/test"));

        let mut cx = Context::from_waker(noop_waker_ref());

        // Initial poll: nothing to do yet, and the connection is kept alive.
        assert!(matches!(handler.poll(&mut cx), Poll::Pending));
        assert!(handler.connection_keep_alive());

        // Wait well past the connection timeout WITHOUT ever delivering a
        // `FullyNegotiatedInbound` (the peer connected but never opened the
        // inbound substream).
        tokio::time::sleep(connection_timeout * 10).await;

        // BUG: `Starting` never armed a timer, so the handler still reports
        // keep-alive and emits no `SubstreamClosed(Timeout)`. The connection is
        // leaked; a correct implementation would have closed it by now.
        assert!(
            matches!(handler.poll(&mut cx), Poll::Pending),
            "Starting state should have timed out and emitted SubstreamClosed, but polls Pending forever"
        );
        assert!(
            handler.connection_keep_alive(),
            "Starting state keeps the connection alive even after the timeout elapsed -> leak"
        );
    }
}
