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
#[serde(default)]
pub struct Config {
    pub backend: BackendSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct BackendSettings {
    pub swarm: SwarmConfig,
    // Initial peers to connect to
    pub initial_peers: Vec<Multiaddr>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SwarmConfig {
    /// Listening IPv4 address
    pub host: Ipv4Addr,
    /// UDP/QUIC listening port. Use 0 for random.
    pub port: u16,
    /// Ed25519 private key in hex format. Default: random.
    #[serde(with = "lb_libp2p::secret_key_serde")]
    pub node_key: SecretKey,

    /// Gossipsub config
    pub gossipsub: gossipsub::Config,

    /// Kademlia config (required; Identify must be enabled too)
    pub kademlia: kademlia::Config,

    /// Identify config (required)
    pub identify: identify::Config,

    /// Chain sync config
    pub chain_sync: chainsync::Config,

    pub nat: nat::Config,
}

impl Default for SwarmConfig {
    fn default() -> Self {
        Self {
            host: Ipv4Addr::UNSPECIFIED,
            port: 0,
            node_key: SecretKey::generate(),
            gossipsub: gossipsub::Config::default(),
            kademlia: kademlia::Config::default(),
            identify: identify::Config::default(),
            chain_sync: chainsync::Config::default(),
            nat: nat::Config::default(),
        }
    }
}
