pub use identify::Settings as IdentifySettings;
pub use kademlia::{CachingSettings, KBucketInserts, Settings as KademliaSettings};
use libp2p::{gossipsub, identity::ed25519};
pub use nat::{
    Settings as NatSettings, TraversalSettings, autonat_client::Settings as AutonatClientSettings,
    gateway::Settings as GatewaySettings, mapping::Settings as NatMappingSettings,
};

use crate::protocol_name::StreamProtocol;

mod identify;
mod kademlia;
mod nat;

#[derive(Debug, Clone)]
pub struct SwarmConfig {
    /// Listening IPv4 address
    pub host: std::net::Ipv4Addr,
    /// UDP/QUIC listening port. Use 0 for random.
    pub port: u16,
    /// Ed25519 private key in hex format. Default: random.
    pub node_key: ed25519::SecretKey,

    /// Gossipsub config
    pub gossipsub_config: gossipsub::Config,

    pub kad_protocol_name: StreamProtocol,
    pub identify_protocol_name: StreamProtocol,
    pub chain_sync_protocol_name: StreamProtocol,

    /// Kademlia config (required; Identify must be enabled too)
    pub kademlia_config: kademlia::Settings,

    /// Identify config (required)
    pub identify_config: identify::Settings,

    /// Chain sync config
    pub chain_sync_config: lb_cryptarchia_sync::Config,

    /// Nat config
    pub nat_config: nat::Settings,
}

pub mod secret_key_serde {
    use libp2p::identity::ed25519;
    use serde::{Deserialize as _, Deserializer, Serialize as _, Serializer, de::Error as _};

    pub fn serialize<S>(key: &ed25519::SecretKey, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let hex_str = hex::encode(key.as_ref());
        hex_str.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<ed25519::SecretKey, D::Error>
    where
        D: Deserializer<'de>,
    {
        let hex_str = String::deserialize(deserializer)?;
        let mut key_bytes = hex::decode(hex_str).map_err(|e| D::Error::custom(format!("{e}")))?;
        ed25519::SecretKey::try_from_bytes(key_bytes.as_mut_slice())
            .map_err(|e| D::Error::custom(format!("{e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Default for SwarmConfig {
        fn default() -> Self {
            Self {
                host: std::net::Ipv4Addr::UNSPECIFIED,
                port: 60000,
                node_key: ed25519::SecretKey::generate(),
                gossipsub_config: gossipsub::Config::default(),
                identify_protocol_name: StreamProtocol::new("/identify/test"),
                kad_protocol_name: StreamProtocol::new("/kademlia/test"),
                chain_sync_protocol_name: StreamProtocol::new("/chainsync/test"),
                kademlia_config: kademlia::Settings::default(),
                identify_config: identify::Settings::default(),
                chain_sync_config: lb_cryptarchia_sync::Config::default(),
                nat_config: nat::Settings::default(),
            }
        }
    }
}
