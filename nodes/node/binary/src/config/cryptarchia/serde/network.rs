use core::{num::NonZeroUsize, time::Duration};
use std::collections::HashSet;

use libp2p::PeerId;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub bootstrap: BootstrapConfig,
    pub sync: SyncConfig,
}

#[serde_as]
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BootstrapConfig {
    pub ibd: IbdConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IbdConfig {
    /// Peers to download blocks from.
    pub peers: HashSet<PeerId>,
    /// Delay before attempting the next download
    /// when no download is needed at the moment from a peer.
    #[serde(default = "default_delay_before_new_download")]
    pub delay_before_new_download: Duration,
}

const fn default_delay_before_new_download() -> Duration {
    Duration::from_secs(10)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SyncConfig {
    pub orphan: OrphanConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OrphanConfig {
    /// The maximum number of pending orphans to keep in the cache.
    #[serde(default = "default_max_orphan_cache_size")]
    pub max_orphan_cache_size: NonZeroUsize,
}

const fn default_max_orphan_cache_size() -> NonZeroUsize {
    NonZeroUsize::new(5).unwrap()
}
