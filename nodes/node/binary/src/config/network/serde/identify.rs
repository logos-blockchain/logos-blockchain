use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// Agent version string to advertise
    /// Default from libp2p: 'rust-libp2p/{version}'
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_version: Option<String>,

    /// Interval in seconds between pushes of identify info
    /// Default from libp2p: 5 minutes (300 seconds)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_secs: Option<u64>,

    /// Whether new/expired listen addresses should trigger
    /// an active push of an identify message to all connected peers
    /// Default from libp2p: false
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_listen_addr_updates: Option<bool>,

    /// How many entries of discovered peers to keep
    /// Default from libp2p: 100
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_size: Option<usize>,

    /// Whether to hide listen addresses in responses (only share external
    /// addresses) Default from libp2p: false
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hide_listen_addrs: Option<bool>,
}
