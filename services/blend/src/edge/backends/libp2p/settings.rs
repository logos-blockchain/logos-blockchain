use core::num::NonZeroU64;

use lb_cryptarchia_engine::EraSchedule;
use lb_libp2p::protocol_name::StreamProtocol;

/// The libp2p edge backend's settings: this node's own, the same in every era,
/// and the protocol of every era.
#[derive(Clone, Debug)]
pub struct Libp2pBlendBackendSettings {
    pub max_dial_attempts_per_peer_per_message: NonZeroU64,
    // $\Phi_{EC}$: the minimum number of connections that the edge node establishes with
    // core nodes to send a single message that needs to be blended.
    pub replication_factor: NonZeroU64,
    /// The protocol core nodes speak in each era.
    pub protocol_names: EraSchedule<StreamProtocol>,
}
