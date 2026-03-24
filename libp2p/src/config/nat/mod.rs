use serde::{Deserialize, Serialize};

pub mod autonat_client;
pub mod gateway;
pub mod mapping;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TraversalSettings {
    pub autonat: autonat_client::Settings,
    pub mapping: mapping::Settings,
    pub gateway_monitor: gateway::Settings,
    /// Optional external address candidate to verify via `AutoNAT`.
    /// Useful for nodes with manual port forwarding where the public
    /// address can't be discovered automatically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_address: Option<libp2p::Multiaddr>,
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
