use std::collections::HashMap;

use lb_libp2p::{Multiaddr, SwarmConfig};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Libp2pConfig {
    pub inner: SwarmConfig,
    /// Runtime-derived application-data limits. This is intentionally skipped
    /// during deserialization and must be rebuilt before starting the backend.
    #[serde(skip)]
    pub max_data_size_by_topic: HashMap<lb_libp2p::gossipsub::TopicHash, usize>,
    // Initial peers to connect to
    #[serde(default)]
    pub initial_peers: Vec<Multiaddr>,
}
