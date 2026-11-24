use core::num::NonZeroU32;

use serde::{Deserialize, Serialize};

/// Bounds on the connections the swarm holds, so that peers opening
/// connections cannot exhaust the node's memory.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Settings {
    /// The most incoming connections that may be in their handshake at once.
    pub max_pending_incoming: NonZeroU32,
    /// The most established incoming connections.
    pub max_established_incoming: NonZeroU32,
    /// The most established connections with one peer, in both directions
    /// together.
    pub max_established_per_peer: NonZeroU32,
    /// The most incoming connections, in their handshake or established, from
    /// one IP address. Loopback addresses are not limited.
    pub max_incoming_per_ip: NonZeroU32,
}
