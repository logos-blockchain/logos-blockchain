use core::num::NonZeroU64;

use lb_libp2p::protocol_name::StreamProtocol;

/// The libp2p edge backend's settings that are the same in every era.
#[derive(Clone, Debug)]
pub struct Libp2pBlendBackendSettings {
    pub max_dial_attempts_per_peer_per_message: NonZeroU64,
    // $\Phi_{EC}$: the minimum number of connections that the edge node establishes with
    // core nodes to send a single message that needs to be blended.
    pub replication_factor: NonZeroU64,
}

/// The libp2p edge backend's settings in an era.
#[derive(Clone, Debug)]
pub struct Libp2pBlendBackendEraSettings {
    /// The protocol core nodes speak in the era.
    pub protocol_name: StreamProtocol,
}
