#![allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "Serde conditional serialization skip requires a specific function signature."
)]

use core::net::Ipv4Addr;

use lb_libp2p::{Multiaddr, ed25519::SecretKey};
use serde::{Deserialize, Serialize};

use crate::config::utils;

pub mod chainsync;
pub mod gossipsub;
pub mod identify;
pub mod kademlia;
pub mod nat;

// Definition copied from the `logos-blockchain-network` service settings,
// assuming the libp2p backend and removing the concrete protocol names, which
// will be injected via the deployment configuration.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Config {
    #[serde(skip_serializing_if = "utils::is_default")]
    pub backend: BackendSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct BackendSettings {
    #[serde(skip_serializing_if = "utils::is_default")]
    pub swarm: SwarmConfig,
    // Initial peers to connect to
    #[serde(skip_serializing_if = "utils::is_default")]
    pub initial_peers: Vec<Multiaddr>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SwarmConfig {
    /// Ed25519 private key in hex format. Default: random.
    #[serde(with = "lb_libp2p::secret_key_serde")]
    pub node_key: SecretKey,

    /// Listening IPv4 address
    #[serde(skip_serializing_if = "is_default_host")]
    pub host: Ipv4Addr,
    /// UDP/QUIC listening port. Use 0 for random.
    #[serde(skip_serializing_if = "is_default_port")]
    pub port: u16,
    /// Gossipsub config
    #[serde(skip_serializing_if = "utils::is_default")]
    pub gossipsub: gossipsub::Config,
    /// Kademlia config (required; Identify must be enabled too)
    #[serde(skip_serializing_if = "utils::is_default")]
    pub kademlia: kademlia::Config,
    /// Identify config (required)
    #[serde(skip_serializing_if = "utils::is_default")]
    pub identify: identify::Config,
    /// Chain sync config
    #[serde(skip_serializing_if = "utils::is_default")]
    pub chain_sync: chainsync::Config,
    #[serde(skip_serializing_if = "utils::is_default")]
    pub nat: nat::Config,
}

const fn default_host() -> Ipv4Addr {
    Ipv4Addr::UNSPECIFIED
}

fn is_default_host(host: &Ipv4Addr) -> bool {
    *host == default_host()
}

const fn default_port() -> u16 {
    0
}

const fn is_default_port(port: &u16) -> bool {
    *port == default_port()
}

impl Default for SwarmConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            node_key: SecretKey::generate(),
            gossipsub: gossipsub::Config::default(),
            kademlia: kademlia::Config::default(),
            identify: identify::Config::default(),
            chain_sync: chainsync::Config::default(),
            nat: nat::Config::default(),
        }
    }
}

impl PartialEq for SwarmConfig {
    fn eq(&self, other: &Self) -> bool {
        let &Self {
            host,
            port,
            node_key,
            gossipsub,
            kademlia,
            identify,
            chain_sync,
            nat,
        } = &self;
        let Self {
            host: other_host,
            port: other_port,
            node_key: other_node_key,
            gossipsub: other_gossipsub,
            kademlia: other_kademlia,
            identify: other_identify,
            chain_sync: other_chain_sync,
            nat: other_nat,
        } = other;

        host == other_host
            && port == other_port
            && node_key.as_ref() == other_node_key.as_ref()
            && gossipsub == other_gossipsub
            && kademlia == other_kademlia
            && identify == other_identify
            && chain_sync == other_chain_sync
            && nat == other_nat
    }
}

impl Eq for SwarmConfig {}
