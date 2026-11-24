use core::time::Duration;

use lb_utils::bounded_duration::{MinimalBoundedDuration, SECOND};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Agent version string to advertise
    pub agent_version: Option<String>,

    /// Interval between pushes of identify info
    /// Default from libp2p
    #[serde_as(as = "Option<MinimalBoundedDuration<1, SECOND>>")]
    pub interval: Option<Duration>,

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
            interval: None,
            push_listen_addr_updates: None,
            cache_size: None,
            hide_listen_addrs: Some(true),
        }
    }
}
