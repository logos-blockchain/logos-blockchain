use core::{num::NonZeroUsize, time::Duration};
use std::collections::HashSet;

use libp2p::PeerId;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub bootstrap: BootstrapConfig,
    pub sync: SyncConfig,
    pub network: NetworkConfig,
}

#[serde_as]
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct BootstrapConfig {
    pub ibd: IbdConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct IbdConfig {
    /// Peers to download blocks from.
    pub peers: HashSet<PeerId>,
    /// Delay before attempting the next download
    /// when no download is needed at the moment from a peer.
    pub delay_before_new_download: Duration,
}

impl Default for IbdConfig {
    fn default() -> Self {
        Self {
            peers: HashSet::default(),
            delay_before_new_download: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct SyncConfig {
    pub orphan: OrphanConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct OrphanConfig {
    /// The maximum number of pending orphans to keep in the cache.
    pub max_orphan_cache_size: NonZeroUsize,
}

impl Default for OrphanConfig {
    fn default() -> Self {
        Self {
            max_orphan_cache_size: NonZeroUsize::new(1000).unwrap(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct NetworkConfig {
    /// The maximum number of connected peers to attempt downloads from
    /// for each target block.
    pub max_connected_peers_to_try_download: usize,
    /// The maximum number of discovered peers to attempt downloads from
    /// for each target block.
    pub max_discovered_peers_to_try_download: usize,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            max_connected_peers_to_try_download: 16,
            max_discovered_peers_to_try_download: 16,
        }
    }
}
