use std::collections::HashMap;

use lb_libp2p::{Multiaddr, SwarmConfig, gossipsub::TopicHash, protocol_name::StreamProtocol};
use lb_time::era::EraSchedule;

pub type Libp2pEraSchedule = EraSchedule<Libp2pEraConfig>;

/// The settings of the libp2p backend: those it keeps for the whole run, and
/// those that change from era to era.
#[derive(Clone, Debug)]
pub struct Libp2pConfig {
    pub inner: SwarmConfig,
    /// Initial peers to connect to.
    pub initial_peers: Vec<Multiaddr>,
    pub era_schedule: Libp2pEraSchedule,
}

/// What the libp2p backend runs with in an era.
#[derive(Clone, Debug)]
pub struct Libp2pEraConfig {
    pub chain_sync_protocol_name: StreamProtocol,
    /// Application-data limits of the era's gossipsub topics.
    pub max_data_size_by_topic: HashMap<TopicHash, usize>,
}
