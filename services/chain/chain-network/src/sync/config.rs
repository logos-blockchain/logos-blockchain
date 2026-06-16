use std::num::{NonZeroU64, NonZeroUsize};

use serde::{Deserialize, Serialize};

const MAX_ORPHAN_CACHE_SIZE: NonZeroUsize =
    NonZeroUsize::new(1000).expect("MAX_ORPHAN_CACHE_SIZE must be non-zero");

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SyncConfig {
    pub orphan: OrphanConfig,
    /// Proactive tip-polling watchdog that catches a node up when it stops
    /// receiving gossiped blocks (e.g. partial eclipse / network partition).
    #[serde(default)]
    pub tip_poll: TipPollConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OrphanConfig {
    /// The maximum number of pending orphans to keep in the cache.
    #[serde(default = "default_max_orphan_cache_size")]
    pub max_orphan_cache_size: NonZeroUsize,
}

/// Configuration for the proactive tip-polling lag watchdog.
///
/// On a slot-tick cadence the node compares its local tip slot against the
/// current slot. If it has fallen behind by more than `lag_threshold_blocks`
/// expected block-intervals (each `1/f` slots, where `f` is the active slot
/// coefficient), it samples up to `max_peers_to_sample` peers with `GetTip` and
/// hands the most-advanced reported tip to the orphan downloader to catch up.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct TipPollConfig {
    /// Whether the proactive tip-polling watchdog is enabled.
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
            lag_threshold_blocks: NonZeroU64::new(3).expect("3 is non-zero"),
            max_peers_to_sample: NonZeroUsize::new(5).expect("5 is non-zero"),
        }
    }
}

const fn default_max_orphan_cache_size() -> NonZeroUsize {
    MAX_ORPHAN_CACHE_SIZE
}
