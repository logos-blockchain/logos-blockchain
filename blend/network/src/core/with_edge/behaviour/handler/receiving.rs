use core::task::{Context, Poll, Waker};
use std::io;

use futures::FutureExt as _;
use libp2p::swarm::ConnectionHandlerEvent;

use crate::{
    core::with_edge::behaviour::handler::{
        ConnectionState, FailureReason, LOG_TARGET, MessageReceiveFuture, PollResult, StateTrait,
        TimerFuture, ToBehaviour, dropped::DroppedState,
    },
    message::IncomingMessage,
};

/// What a completed read amounts to: the message it delivered, or the reason
/// the connection is being dropped without one.
fn classify_read_outcome(
    result: io::Result<IncomingMessage>,
) -> Result<IncomingMessage, FailureReason> {
    match result {
        Ok(message) => {
            tracing::trace!(target: LOG_TARGET, "Message received successfully. Transitioning from `Receiving` to `Dropped`.");
            Ok(message)
        }
        // The edge node closed or lost its stream before a whole message
        // arrived. It holds no peering slot and the connection was only ever
        // going to carry the one message, so there is nothing to do but drop
        // it.
        Err(error) => {
            tracing::debug!(target: LOG_TARGET, "Edge node's stream ended before a message arrived: {error}");
            Err(FailureReason::MessageStream)
        }
    }
}

/// State representing the moment in which a new message is being received from
/// a peer.
pub struct ReceivingState {
    /// The future that will resolve when the attempt to receive the message is
    /// completed, either successfully or with an error.
    incoming_message: MessageReceiveFuture,
    /// The timer future that will be polled regularly to close the connection
    /// if receiving the message takes too long.
    timeout_timer: TimerFuture,
    waker: Option<Waker>,
}

impl ReceivingState {
    pub fn new(
        timeout_timer: TimerFuture,
        incoming_message: MessageReceiveFuture,
        waker: Option<Waker>,
    ) -> Self {
        // We wake here since the future to receive the message must be polled once to
        // start making any progress.
        if let Some(waker) = waker {
            waker.wake();
        }
        Self {
            incoming_message,
            timeout_timer,
            waker: None,
        }
    }
}

impl From<ReceivingState> for ConnectionState {
    fn from(value: ReceivingState) -> Self {
        Self::Receiving(value)
    }
}

impl StateTrait for ReceivingState {
    // If the timer elapses, moves the state machine to `DroppedState` with a
    // timeout error. Otherwise, if the message is correctly received it generates a
    // new message, else it moves to the `DroppedState` with the message reception
    // error.
    fn poll(mut self, cx: &mut Context<'_>) -> PollResult<ConnectionState> {
        let Poll::Pending = self.timeout_timer.poll_unpin(cx) else {
            tracing::debug!(target: LOG_TARGET, "Timeout reached without completing the reception of the message. Closing the connection.");
            return (
                Poll::Pending,
                DroppedState::new(Some(FailureReason::Timeout), Some(cx.waker().clone())).into(),
            );
        };
        let Poll::Ready(message_receive_result) = self.incoming_message.poll_unpin(cx) else {
            // We don't wake here since the `incoming_message` future will wake the waker
            // when completed.
            self.waker = Some(cx.waker().clone());
            return (Poll::Pending, self.into());
        };
        match classify_read_outcome(message_receive_result) {
            Ok(message) => (
                Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                    ToBehaviour::Message(message),
                )),
                DroppedState::new(None, Some(cx.waker().clone())).into(),
            ),
            Err(failure) => (
                Poll::Pending,
                DroppedState::new(Some(failure), Some(cx.waker().clone())).into(),
            ),
        }
    }

    fn take_waker(&mut self) -> Option<Waker> {
        self.waker.take()
    }
}
