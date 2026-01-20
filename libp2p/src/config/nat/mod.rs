use serde::{Deserialize, Serialize};

pub mod autonat_client;
pub mod gateway;
pub mod mapping;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TraversalSettings {
    pub autonat: autonat_client::Settings,
    pub mapping: mapping::Settings,
    pub gateway_monitor: gateway::Settings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Settings {
    /// NAT traversal with autonat, mapping, and gateway monitoring
    Traversal(TraversalSettings),
    /// Static external address for nodes with fixed public IPs
    Static {
        /// The fixed external address to use (NAT traversal disabled)
        external_address: libp2p::Multiaddr,
    },
}

impl Default for Settings {
    fn default() -> Self {
        Self::Traversal(TraversalSettings::default())
    }
}
