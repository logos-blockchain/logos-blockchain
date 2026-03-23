use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Agent version string to advertise
    pub agent_version: Option<String>,

    /// Interval in seconds between pushes of identify info
    /// Default from libp2p
    pub interval_secs: Option<u64>,

    /// Whether new/expired listen addresses should trigger
    /// an active push of an identify message to all connected peers
    pub push_listen_addr_updates: Option<bool>,

    /// How many entries of discovered peers to keep
    pub cache_size: Option<usize>,

    /// Whether to hide listen addresses in responses (only share external
    /// addresses)
    pub hide_listen_addrs: Option<bool>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            agent_version: None,
            interval_secs: None,
            push_listen_addr_updates: None,
            cache_size: None,
            hide_listen_addrs: Some(true),
        }
    }
}
