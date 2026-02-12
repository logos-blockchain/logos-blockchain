use core::{num::NonZeroU64, ops::RangeInclusive, time::Duration};

use lb_key_management_system_service::backend::preload::KeyId;
use lb_libp2p::Multiaddr;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub backend: BackendConfig,
    pub zk: ZkSettings,
}

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackendConfig {
    #[serde(default = "default_listening_address")]
    pub listening_address: Multiaddr,
    #[serde(default = "default_core_peering_degree")]
    pub core_peering_degree: RangeInclusive<u64>,
    #[serde_as(
        as = "lb_utils::bounded_duration::MinimalBoundedDuration<1, lb_utils::bounded_duration::SECOND>"
    )]
    #[serde(default = "default_edge_node_connection_timeout")]
    pub edge_node_connection_timeout: Duration,
    #[serde(default = "default_max_edge_node_incoming_connections")]
    pub max_edge_node_incoming_connections: u64,
    #[serde(default = "default_max_dial_attempts_per_peer")]
    pub max_dial_attempts_per_peer: NonZeroU64,
}

const fn default_listening_address() -> Multiaddr {
    "/ip4/0.0.0.0/udp/10000/quic-v1".parse().unwrap()
}

const fn default_core_peering_degree() -> RangeInclusive<u64> {
    1..=3
}

const fn default_edge_node_connection_timeout() -> Duration {
    Duration::from_secs(1)
}

const fn default_max_edge_node_incoming_connections() -> u64 {
    300
}

const fn default_max_dial_attempts_per_peer() -> NonZeroU64 {
    NonZeroU64::new(3).unwrap()
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            listening_address: default_listening_address(),
            core_peering_degree: default_core_peering_degree(),
            edge_node_connection_timeout: default_edge_node_connection_timeout(),
            max_edge_node_incoming_connections: default_max_edge_node_incoming_connections(),
            max_dial_attempts_per_peer: default_max_dial_attempts_per_peer(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ZkSettings {
    pub secret_key_kms_id: KeyId,
}
