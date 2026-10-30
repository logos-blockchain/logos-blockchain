use core::net::Ipv4Addr;

use lb_libp2p::{Multiaddr, ed25519::SecretKey};
use serde::{Deserialize, Serialize};

pub mod chainsync;
pub mod gossipsub;
pub mod identify;
pub mod kademlia;
pub mod nat;

// Definition copied from the `logos-blockchain-network` service settings,
// assuming the libp2p backend and removing the concrete protocol names, which
// will be injected via the deployment configuration.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Config {
    pub backend: BackendSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BackendSettings {
    pub swarm: SwarmConfig,
    // Initial peers to connect to
    #[serde(default)]
    pub initial_peers: Vec<Multiaddr>,
}

/// Every value but the node key has a default, so a config file only has to
/// carry the key: the node identity is never made up on load.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmConfig {
    /// Listening IPv4 address
    #[serde(default = "SwarmConfig::default_host")]
    pub host: Ipv4Addr,
    /// UDP/QUIC listening port. Use 0 for random.
    #[serde(default = "SwarmConfig::default_port")]
    pub port: u16,
    /// Ed25519 private key in hex format.
    #[serde(with = "lb_libp2p::secret_key_serde")]
    pub node_key: SecretKey,

    /// Gossipsub config
    #[serde(default)]
    pub gossipsub: gossipsub::Config,

    /// Kademlia config (required; Identify must be enabled too)
    #[serde(default)]
    pub kademlia: kademlia::Config,

    /// Identify config (required)
    #[serde(default)]
    pub identify: identify::Config,

    /// Chain sync config
    #[serde(default)]
    pub chain_sync: chainsync::Config,

    #[serde(default)]
    pub nat: nat::Config,
}

impl SwarmConfig {
    #[must_use]
    pub const fn default_host() -> Ipv4Addr {
        Ipv4Addr::UNSPECIFIED
    }

    #[must_use]
    pub const fn default_port() -> u16 {
        3000
    }
}

impl Default for SwarmConfig {
    fn default() -> Self {
        Self {
            host: Self::default_host(),
            port: Self::default_port(),
            node_key: SecretKey::generate(),
            gossipsub: gossipsub::Config::default(),
            kademlia: kademlia::Config::default(),
            identify: identify::Config::default(),
            chain_sync: chainsync::Config::default(),
            nat: nat::Config::default(),
        }
    }
}
