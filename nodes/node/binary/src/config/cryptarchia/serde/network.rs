use core::{
    num::{NonZeroU64, NonZeroUsize},
    time::Duration,
};
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
    /// Peers to query for the chain tip during IBD.
    pub peers: HashSet<PeerId>,
    /// Deprecated: no longer used. Kept for YAML backward compatibility.
    pub delay_before_new_download: Duration,
    /// Maximum number of attempts when fetching tips from IBD peers.
    pub tips_fetch_max_attempts: usize,
    /// Lower bound of the exponential backoff between tip-fetch attempts.
    pub tips_fetch_min_delay: Duration,
    /// Upper bound of the exponential backoff between tip-fetch attempts.
    pub tips_fetch_max_delay: Duration,
    /// Delay between IBD rounds.
    pub round_delay: Duration,
}

impl Default for IbdConfig {
    fn default() -> Self {
        Self {
            peers: HashSet::new(),
            delay_before_new_download: Duration::from_secs(10),
            tips_fetch_max_attempts: 3,
            tips_fetch_min_delay: Duration::from_millis(250),
            tips_fetch_max_delay: Duration::from_secs(1),
            round_delay: Duration::from_secs(1),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct SyncConfig {
    pub orphan: OrphanConfig,
    pub tip_poll: TipPollConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct OrphanConfig {
    /// The maximum number of pending orphans to keep in the cache.
    pub max_orphan_cache_size: NonZeroUsize,
    /// The maximum number of block IDs to remember in the rejected-blocks
    /// negative cache. The orphan pipeline consults this cache to short-circuit
    /// known-bad/older-than-LIB blocks before enqueuing or downloading them.
    ///
    /// Setting this to `0` disables the cache entirely.
    pub max_rejected_cache_size: usize,
}

impl Default for OrphanConfig {
    fn default() -> Self {
        Self {
            max_orphan_cache_size: NonZeroUsize::new(1000).unwrap(),
            max_rejected_cache_size: 1000,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct TipPollConfig {
    /// Whether the proactive tip-polling lag watchdog is enabled.
    pub enabled: bool,
    /// How many expected block-intervals (`1/f` slots each) the local tip may
    /// lag behind the current slot before we proactively poll peers.
    pub lag_threshold_blocks: NonZeroU64,
    /// Maximum number of peers to sample with `GetTip` on each poll.
    pub max_peers_to_sample: NonZeroUsize,
}

impl Default for TipPollConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            lag_threshold_blocks: NonZeroU64::new(3).unwrap(),
            max_peers_to_sample: NonZeroUsize::new(5).unwrap(),
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
