use core::{
    pin::Pin,
    task::{Context, Poll, Waker},
    time::Duration,
};
use std::io;

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

const LOG_TARGET: &str = "blend::network::core::handler::core-edge";

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
