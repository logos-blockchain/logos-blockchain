//! Proof for finding `20261103T082057-ed7933/5`.
//!
//! Blend Protocol, Network Maintenance, Connectivity Maintenance, Liveness:
//!
//! > 1. A connection with a core node is **live** when the neighbor has
//! >    delivered a message, duplicate or not, within its last `W` connected
//! >    rounds, or has been connected for fewer than `W` rounds in total.
//!
//! and, Degree 3, "A connection that is not live is closed."
//!
//! `PeerLivenessMap` is crate-private, so this test compiles the unit's own
//! `liveness.rs` (unchanged, at the scanned commit) into the test crate. The
//! module path below is the one that file's own unit tests refer to, so they
//! compile and run here too.
//!
//! The map counts the round boundaries a neighbour has been connected across,
//! `d`, and the value of `d` at its last delivery, `m`, and calls the neighbour
//! live iff `d − m < W` (or `d < W` with no delivery). A delivery in the round
//! the connection came up (`m = 0`) is therefore worth exactly as much as no
//! delivery at all: both neighbours stop being live at `d = W`. No consistent
//! count of connected rounds gives that:
//!
//! - counting the round that has just begun as connected, the neighbour that
//!   never delivered has been connected for `W` rounds at `d = W − 1`, so it
//!   is not live there, yet the map keeps it;
//! - not counting it, the last `W` connected rounds at `d = W` are rounds
//!   `0..W`, which include the delivery, so the neighbour that delivered is
//!   still live there, yet the map closes it.

#[path = "../src/core/with_core/behaviour/liveness.rs"]
#[allow(dead_code, unused_imports)]
pub mod liveness;

/// The path `liveness.rs`'s own unit tests import from.
mod core {
    pub mod with_core {
        pub mod behaviour {
            pub use crate::liveness;
        }
    }
}

use std::num::NonZeroU128;

use lb_blend_primitives::time::RoundCount;
use libp2p::PeerId;

use crate::liveness::PeerLivenessMap;

/// `W`, the observation window, as deployed (`10·Δ_max` with `Δ_max = 1`).
const W: u128 = 10;

/// Whether `peer` is still live after its connection has crossed `rounds`
/// round boundaries.
fn live_after(
    rounds: u128,
    peer: PeerId,
    delivers_in_first_round: bool,
) -> bool {
    let mut liveness = PeerLivenessMap::new(RoundCount::new(NonZeroU128::new(W).unwrap()));
    // The connection comes up...
    liveness.start_or_resume_observing(peer);
    // ... and, maybe, the neighbour delivers a message in that same round.
    if delivers_in_first_round {
        liveness.record_message_from_neighbour(peer);
    }
    for _ in 0..rounds {
        liveness.enter_new_round_with_peers(std::iter::once(&peer));
    }
    !liveness.is_connection_unhealthy(&peer)
}

#[test]
fn a_delivery_in_the_first_connected_round_is_not_worth_nothing() {
    let delivering = PeerId::random();
    let silent = PeerId::random();

    // Controls: the window itself is honoured on both sides.
    assert!(live_after(W - 2, silent, false));
    assert!(!live_after(W + 1, delivering, true));

    let silent_kept_at_w_minus_1 = live_after(W - 1, silent, false);
    let delivering_kept_at_w = live_after(W, delivering, true);

    assert!(
        delivering_kept_at_w || !silent_kept_at_w_minus_1,
        "liveness is one round off in one of its two clauses: a neighbour that never \
         delivered is still live after {} round boundaries ({silent_kept_at_w_minus_1}), and \
         a neighbour that delivered in the round its connection came up is no longer live after \
         {W} ({delivering_kept_at_w}). Both are closed at the same boundary, so the delivery \
         counted for nothing.",
        W - 1
    );
}
