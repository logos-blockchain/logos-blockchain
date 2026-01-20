use core::task::{Context, Poll, Waker};

use libp2p::swarm::ConnectionHandlerEvent;

use crate::core::with_edge::behaviour::handler::{
    ConnectionState, FailureReason, PollResult, StateTrait, ToBehaviour,
};

enum ErrorState {
    NotConsumed(Option<FailureReason>),
    Consumed,
}

/// State indicating either that an error should be emitted, or that the state
/// machine has reached its end state, from which it does not exit anymore.
pub struct DroppedState {
    error: ErrorState,
}

impl DroppedState {
    pub fn new(error: Option<FailureReason>, waker: Option<Waker>) -> Self {
        // We wake here because we want to return the result of this.
        if let Some(waker) = waker {
            waker.wake();
        }
        Self {
            error: ErrorState::NotConsumed(error),
        }
    }
}

impl From<DroppedState> for ConnectionState {
    fn from(value: DroppedState) -> Self {
        Self::Dropped(value)
    }
}

impl StateTrait for DroppedState {
    // If an error message is to be emitted, it returns `Poll::Ready`, consuming it.
    // After an error is consumed or if no error is to be consumed, the state
    // machine will indefinitely return `Poll::Pending` every time it is polled.
    fn poll(self, _cx: &mut Context<'_>) -> PollResult<ConnectionState> {
        let ErrorState::NotConsumed(error) = self.error else {
            return (Poll::Pending, self.into());
        };
        let poll_result =
            ConnectionHandlerEvent::NotifyBehaviour(ToBehaviour::SubstreamClosed(error));
        (
            Poll::Ready(poll_result),
            Self {
                error: ErrorState::Consumed,
            }
            .into(),
        )
    }

    fn take_waker(&mut self) -> Option<Waker> {
        None
    }
}
