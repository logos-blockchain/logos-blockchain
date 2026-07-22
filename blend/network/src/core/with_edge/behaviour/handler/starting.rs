use core::{
    task::{Context, Poll, Waker},
    time::Duration,
};

use futures::FutureExt as _;
use futures_timer::Delay;
use libp2p::swarm::handler::FullyNegotiatedInbound;

use crate::core::with_edge::behaviour::handler::{
    ConnectionEvent, ConnectionState, FailureReason, LOG_TARGET, PollResult, StateTrait,
    TimerFuture, dropped::DroppedState, ready_to_receive::ReadyToReceiveState,
};

/// Entrypoint to start receiving a single message from an edge node.
pub struct StartingState {
    /// Timer armed as soon as this state is entered. It bounds the *total* time
    /// budget to receive a message: if it elapses before the inbound stream is
    /// negotiated, the connection is closed so it cannot be leaked by a peer
    /// that connects but never opens the substream. Once negotiated, the same
    /// timer is handed over to `ReadyToReceive`, so the deadline is not reset
    /// by the state transition.
    timeout_timer: TimerFuture,
    /// The waker to wake when we need to force a new round of polling to
    /// progress the state machine.
    waker: Option<Waker>,
}

impl StartingState {
    pub fn new(connection_timeout: Duration) -> Self {
        Self {
            timeout_timer: Box::pin(Delay::new(connection_timeout)),
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
                ReadyToReceiveState::new(self.timeout_timer, inbound_stream, self.waker.take())
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

    // If the timer elapses before the inbound stream is negotiated, moves the
    // state machine to `DroppedState` with a timeout error so the connection is
    // closed. Otherwise it just stores the waker and stays in `Starting`.
    fn poll(mut self, cx: &mut Context<'_>) -> PollResult<ConnectionState> {
        if self.timeout_timer.poll_unpin(cx).is_ready() {
            tracing::debug!(target: LOG_TARGET, "Timeout reached without negotiating the inbound stream. Closing the connection.");
            return (
                Poll::Pending,
                DroppedState::new(Some(FailureReason::Timeout), Some(cx.waker().clone())).into(),
            );
        }
        self.waker = Some(cx.waker().clone());
        (Poll::Pending, self.into())
    }

    fn take_waker(&mut self) -> Option<Waker> {
        self.waker.take()
    }
}
