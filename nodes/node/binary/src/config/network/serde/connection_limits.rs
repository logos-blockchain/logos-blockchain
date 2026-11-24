use core::num::NonZeroU32;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
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

impl Default for Config {
    fn default() -> Self {
        Self {
            max_pending_incoming: 128.try_into().unwrap(),
            // About 90 MB of connection state, at under 200 kB a connection.
            max_established_incoming: 512.try_into().unwrap(),
            // Two peers can hold several connections at once: each may dial
            // the other both by address and through Kademlia, and an AutoNAT
            // dial-back opens one of its own. The limits per IP address and in
            // total are what bound a host, whose peer identities cost nothing.
            max_established_per_peer: 8.try_into().unwrap(),
            max_incoming_per_ip: 8.try_into().unwrap(),
        }
    }
}

impl From<Config> for lb_libp2p::ConnectionLimitsSettings {
    fn from(config: Config) -> Self {
        Self {
            max_pending_incoming: config.max_pending_incoming,
            max_established_incoming: config.max_established_incoming,
            max_established_per_peer: config.max_established_per_peer,
            max_incoming_per_ip: config.max_incoming_per_ip,
        }
    }
}
