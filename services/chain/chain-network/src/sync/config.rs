use std::num::{NonZeroU64, NonZeroUsize};

use serde::{Deserialize, Serialize};

const MAX_ORPHAN_CACHE_SIZE: NonZeroUsize =
    NonZeroUsize::new(1000).expect("MAX_ORPHAN_CACHE_SIZE must be non-zero");

const DEFAULT_TIP_POLL_LAG_THRESHOLD_BLOCKS: NonZeroU64 =
    NonZeroU64::new(3).expect("DEFAULT_TIP_POLL_LAG_THRESHOLD_BLOCKS must be non-zero");

const DEFAULT_TIP_POLL_MAX_PEERS_TO_SAMPLE: NonZeroUsize =
    NonZeroUsize::new(5).expect("DEFAULT_TIP_POLL_MAX_PEERS_TO_SAMPLE must be non-zero");

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
pub struct TipPollConfig {
    /// Whether the proactive tip-polling watchdog is enabled.
    #[serde(default = "default_tip_poll_enabled")]
    pub enabled: bool,
    /// How many expected block-intervals (`1/f` slots each) the local tip may
    /// lag behind the current slot before we proactively poll peers.
    #[serde(default = "default_tip_poll_lag_threshold_blocks")]
    pub lag_threshold_blocks: NonZeroU64,
    /// Maximum number of peers to sample with `GetTip` on each poll.
    #[serde(default = "default_tip_poll_max_peers_to_sample")]
    pub max_peers_to_sample: NonZeroUsize,
}

impl Default for TipPollConfig {
    fn default() -> Self {
        Self {
            enabled: default_tip_poll_enabled(),
            lag_threshold_blocks: default_tip_poll_lag_threshold_blocks(),
            max_peers_to_sample: default_tip_poll_max_peers_to_sample(),
        }
    }
}

const fn default_max_orphan_cache_size() -> NonZeroUsize {
    MAX_ORPHAN_CACHE_SIZE
}

const fn default_tip_poll_enabled() -> bool {
    true
}

const fn default_tip_poll_lag_threshold_blocks() -> NonZeroU64 {
    DEFAULT_TIP_POLL_LAG_THRESHOLD_BLOCKS
}

const fn default_tip_poll_max_peers_to_sample() -> NonZeroUsize {
    DEFAULT_TIP_POLL_MAX_PEERS_TO_SAMPLE
}
