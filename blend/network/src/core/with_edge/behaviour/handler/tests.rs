use core::{
    task::{Context, Poll},
    time::Duration,
};

use futures::task::noop_waker_ref;
use libp2p::{
    StreamProtocol,
    swarm::{ConnectionHandler as _, ConnectionHandlerEvent},
};

use crate::core::with_edge::behaviour::handler::{ConnectionHandler, FailureReason, ToBehaviour};

/// Regression test for audit finding #9: an edge connection that never opens
/// its inbound substream must be closed once the connection timeout elapses,
/// instead of being leaked.
///
/// The handler starts in `Starting`. It now arms a timeout timer on entry,
/// so a peer that establishes the connection but never opens the inbound
/// substream no longer pins the handler in `Starting` forever. When the
/// timer fires, `Starting` transitions to `Dropped`: `poll` yields
/// `SubstreamClosed(Timeout)` and `connection_keep_alive()` drops to
/// `false`, letting the swarm reclaim the connection.
///
/// Before the fix this leaked: `Starting::poll` only stored the waker and
/// returned `Pending`, and `connection_keep_alive()` stayed `true` for every
/// state except `Dropped`, so a remote peer could pin unbounded connections
/// (FD/memory exhaustion).
#[tokio::test]
async fn starting_state_without_inbound_substream_times_out() {
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

    // The timer has elapsed: `Starting` transitions to `Dropped`. This first
    // poll returns `Pending` while performing the transition, but keep-alive
    // already drops so the swarm stops pinning the connection.
    assert!(matches!(handler.poll(&mut cx), Poll::Pending));
    assert!(
        !handler.connection_keep_alive(),
        "connection should no longer be kept alive after the timeout elapsed"
    );

    // The subsequent poll emits the `SubstreamClosed(Timeout)` notification.
    assert!(
        matches!(
            handler.poll(&mut cx),
            Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                ToBehaviour::SubstreamClosed(Some(FailureReason::Timeout))
            ))
        ),
        "Starting state should emit SubstreamClosed(Timeout) after the timeout elapses"
    );
}
