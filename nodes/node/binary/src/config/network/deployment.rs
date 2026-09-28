use lb_libp2p::protocol_name::StreamProtocol;
use serde::{Deserialize, Serialize};

// TODO: These will be removed from deployment and entirely derived from the
// fork digest in a follow-up PR.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Settings {
    pub kademlia_protocol_name: StreamProtocol,
    pub identify_protocol_name: StreamProtocol,
    pub chain_sync_protocol_name: StreamProtocol,
    pub blend_protocol_name: StreamProtocol,
    pub cryptarchia_topic: String,
    pub mempool_topic: String,
}
