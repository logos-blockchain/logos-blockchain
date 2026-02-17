#![allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "Serde conditional serialization skip requires a specific function signature."
)]

use core::{num::NonZeroU64, ops::RangeInclusive, time::Duration};

use lb_key_management_system_service::backend::preload::KeyId;
use lb_libp2p::Multiaddr;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use crate::config::utils;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    pub zk: ZkSettings,

    #[serde(default)]
    #[serde(skip_serializing_if = "utils::is_default")]
    pub backend: BackendConfig,
}

#[serde_as]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BackendConfig {
    #[serde(skip_serializing_if = "is_default_listening_address")]
    pub listening_address: Multiaddr,
    #[serde(skip_serializing_if = "is_default_core_peering_degree")]
    pub core_peering_degree: RangeInclusive<u64>,
    #[serde_as(
        as = "lb_utils::bounded_duration::MinimalBoundedDuration<1, lb_utils::bounded_duration::SECOND>"
    )]
    #[serde(skip_serializing_if = "is_default_edge_node_connection_timeout")]
    pub edge_node_connection_timeout: Duration,
    #[serde(skip_serializing_if = "is_default_max_edge_node_incoming_connections")]
    pub max_edge_node_incoming_connections: u64,
    #[serde(skip_serializing_if = "is_default_max_dial_attempts_per_peer")]
    pub max_dial_attempts_per_peer: NonZeroU64,
}

fn default_listening_address() -> Multiaddr {
    "/ip4/0.0.0.0/udp/10000/quic-v1".parse().unwrap()
}

fn is_default_listening_address(addr: &Multiaddr) -> bool {
    *addr == default_listening_address()
}

const fn default_core_peering_degree() -> RangeInclusive<u64> {
    1..=3
}

fn is_default_core_peering_degree(degree: &RangeInclusive<u64>) -> bool {
    *degree == default_core_peering_degree()
}

const fn default_edge_node_connection_timeout() -> Duration {
    Duration::from_secs(1)
}

fn is_default_edge_node_connection_timeout(timeout: &Duration) -> bool {
    *timeout == default_edge_node_connection_timeout()
}

const fn default_max_edge_node_incoming_connections() -> u64 {
    300
}

const fn is_default_max_edge_node_incoming_connections(max: &u64) -> bool {
    *max == default_max_edge_node_incoming_connections()
}

const fn default_max_dial_attempts_per_peer() -> NonZeroU64 {
    NonZeroU64::new(1).unwrap()
}

fn is_default_max_dial_attempts_per_peer(value: &NonZeroU64) -> bool {
    *value == default_max_dial_attempts_per_peer()
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

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ZkSettings {
    pub secret_key_kms_id: KeyId,
}
