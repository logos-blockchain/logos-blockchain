use core::{num::NonZeroU64, ops::RangeInclusive, time::Duration};

use lb_key_management_system_service::backend::preload::KeyId;
use lb_libp2p::Multiaddr;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub backend: BackendConfig,
    pub zk: ZkSettings,
}

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct BackendConfig {
    pub listening_address: Multiaddr,
    pub core_peering_degree: RangeInclusive<u64>,
    #[serde_as(
        as = "lb_utils::bounded_duration::MinimalBoundedDuration<1, lb_utils::bounded_duration::SECOND>"
    )]
    pub edge_node_connection_timeout: Duration,
    pub max_edge_node_incoming_connections: u64,
    pub max_dial_attempts_per_peer: NonZeroU64,
}

impl BackendConfig {
    #[must_use]
    pub const fn default_port() -> u16 {
        3400
    }

    /// # Panics
    ///
    /// This function will panic if the constructed multiaddr string is invalid.
    #[must_use]
    pub fn default_listening_address(port: u16) -> Multiaddr {
        format!("/ip4/0.0.0.0/udp/{port}/quic-v1")
            .parse()
            .expect("Valid multiaddr structure")
    }
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            listening_address: Self::default_listening_address(Self::default_port()),
            core_peering_degree: 3..=5,
            edge_node_connection_timeout: Duration::from_secs(1),
            max_edge_node_incoming_connections: 300,
            max_dial_attempts_per_peer: NonZeroU64::new(3).unwrap(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ZkSettings {
    pub secret_key_kms_id: KeyId,
}
