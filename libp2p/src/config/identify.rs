use std::time::Duration;

use libp2p::{StreamProtocol, identify, identity};
use serde::{Deserialize, Serialize};

/// A serializable representation of Identify configuration options.
/// When a value is None, the libp2p defaults are used.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
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

impl Settings {
    #[must_use]
    pub fn to_libp2p_config(
        &self,
        public_key: identity::PublicKey,
        protocol_name: &StreamProtocol,
    ) -> identify::Config {
        let mut config = identify::Config::new(protocol_name.to_string(), public_key);

        // Apply only the settings that were specified, leaving libp2p defaults for the
        // rest
        if let Some(agent_version) = &self.agent_version {
            config = config.with_agent_version(agent_version.clone());
        }

        if let Some(interval) = self.interval_secs {
            config = config.with_interval(Duration::from_secs(interval));
        }

        if let Some(push_updates) = self.push_listen_addr_updates {
            config = config.with_push_listen_addr_updates(push_updates);
        }

        if let Some(cache_size) = self.cache_size {
            config = config.with_cache_size(cache_size);
        }

        if let Some(hide_addrs) = self.hide_listen_addrs {
            config = config.with_hide_listen_addrs(hide_addrs);
        }

        config
    }
}
