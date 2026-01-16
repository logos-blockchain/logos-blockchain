use core::{
    task::{Context, Poll, Waker},
    time::Duration,
};

use futures_timer::Delay;
use libp2p::swarm::handler::FullyNegotiatedInbound;

use crate::core::with_edge::behaviour::handler::{
    ConnectionEvent, ConnectionState, FailureReason, LOG_TARGET, PollResult, StateTrait,
    dropped::DroppedState, ready_to_receive::ReadyToReceiveState,
};

/// Entrypoint to start receiving a single message from an edge node.
pub struct StartingState {
    /// Time after which the connection will be closed. It is started as soon as
    /// the inbound stream is negotiated.
    connection_timeout: Duration,
    /// The waker to wake when we need to force a new round of polling to
    /// progress the state machine.
    waker: Option<Waker>,
}

impl StartingState {
    pub const fn new(connection_timeout: Duration) -> Self {
        Self {
            connection_timeout,
            waker: None,
        }
    }
}

impl From<StartingState> for ConnectionState {
    fn from(value: StartingState) -> Self {
        Self::Starting(value)
    }
}

impl StateTrait for StartingState {
    // Moves the state machine to `ReadyToReceive` upon receiving a
    // `FullyNegotiatedInbound`. In case of `ListenUpgradeError`, the state machine
    // is moved to the `DroppedState` state (the last state), with the relative
    // error.
    fn on_connection_event(mut self, event: ConnectionEvent) -> ConnectionState {
        match event {
            ConnectionEvent::FullyNegotiatedInbound(FullyNegotiatedInbound {
                protocol: inbound_stream,
                ..
            }) => {
                tracing::trace!(target: LOG_TARGET, "Transitioning from `Starting` to `ReadyToReceive`.");
                ReadyToReceiveState::new(
                    Box::pin(Delay::new(self.connection_timeout)),
                    inbound_stream,
                    self.waker.take(),
                )
                .into()
            }
            ConnectionEvent::ListenUpgradeError(error) => {
                tracing::trace!(target: LOG_TARGET, "Inbound upgrade error: {error:?}");
                DroppedState::new(Some(FailureReason::UpgradeError), self.waker.take()).into()
            }
            unprocessed_event => {
                tracing::trace!(target: LOG_TARGET, "Ignoring connection event {unprocessed_event:?}");
                // We don't need to wake since nothing really happened here.
                self.into()
            }
        }
    }

    // No state machine changes in here.
    fn poll(mut self, cx: &mut Context<'_>) -> PollResult<ConnectionState> {
        self.waker = Some(cx.waker().clone());
        (Poll::Pending, self.into())
    }

    fn take_waker(&mut self) -> Option<Waker> {
        self.waker.take()
    }
}
