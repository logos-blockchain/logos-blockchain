use nomos_libp2p::{
    IdentifySettings, KademliaSettings, Multiaddr, NatSettings, cryptarchia_sync, ed25519,
    gossipsub,
};
use serde::{Deserialize, Serialize};

// Definition copied from the `nomos-network` service settings, assuming the
// libp2p backend and removing the concrete protocol names, which will be
// injected via the deployment configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub backend: BackendSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendSettings {
    pub swarm: SwarmConfig,
    // Initial peers to connect to
    #[serde(default)]
    pub initial_peers: Vec<Multiaddr>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmConfig {
    /// Listening IPv4 address
    pub host: std::net::Ipv4Addr,
    /// UDP/QUIC listening port. Use 0 for random.
    pub port: u16,
    /// Ed25519 private key in hex format. Default: random.
    #[serde(
        with = "nomos_libp2p::secret_key_serde",
        default = "ed25519::SecretKey::generate"
    )]
    pub node_key: ed25519::SecretKey,

    /// Gossipsub config
    #[serde(
        with = "nomos_libp2p::config::gossipsub::ConfigDef",
        default = "nomos_libp2p::gossipsub::Config::default"
    )]
    pub gossipsub_config: gossipsub::Config,

    /// Kademlia config (required; Identify must be enabled too)
    #[serde(default)]
    pub kademlia_config: KademliaSettings,

    /// Identify config (required)
    #[serde(default)]
    pub identify_config: IdentifySettings,

    /// Chain sync config
    #[serde(default)]
    pub chain_sync_config: cryptarchia_sync::Config,

    /// Nat config
    #[serde(default)]
    pub nat_config: NatSettings,
}
