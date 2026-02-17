#![allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "Serde conditional serialization skip requires a specific function signature."
)]

use core::{num::NonZeroUsize, time::Duration};
use std::collections::HashSet;

use libp2p::PeerId;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use crate::config::utils;

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Config {
    #[serde(skip_serializing_if = "utils::is_default")]
    pub bootstrap: BootstrapConfig,
    #[serde(skip_serializing_if = "utils::is_default")]
    pub sync: SyncConfig,
}

#[serde_as]
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct BootstrapConfig {
    #[serde(skip_serializing_if = "utils::is_default")]
    pub ibd: IbdConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct IbdConfig {
    /// Peers to download blocks from.
    #[serde(skip_serializing_if = "utils::is_default")]
    pub peers: HashSet<PeerId>,
    /// Delay before attempting the next download
    /// when no download is needed at the moment from a peer.
    #[serde(skip_serializing_if = "is_default_delay_before_new_download")]
    pub delay_before_new_download: Duration,
}

const fn default_delay_before_new_download() -> Duration {
    Duration::from_secs(10)
}

fn is_default_delay_before_new_download(value: &Duration) -> bool {
    *value == default_delay_before_new_download()
}

impl Default for IbdConfig {
    fn default() -> Self {
        Self {
            peers: HashSet::default(),
            delay_before_new_download: default_delay_before_new_download(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct SyncConfig {
    #[serde(skip_serializing_if = "utils::is_default")]
    pub orphan: OrphanConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct OrphanConfig {
    /// The maximum number of pending orphans to keep in the cache.
    #[serde(skip_serializing_if = "is_default_max_orphan_cache_size")]
    pub max_orphan_cache_size: NonZeroUsize,
}

const fn default_max_orphan_cache_size() -> NonZeroUsize {
    NonZeroUsize::new(5).unwrap()
}

fn is_default_max_orphan_cache_size(value: &NonZeroUsize) -> bool {
    *value == default_max_orphan_cache_size()
}

impl Default for OrphanConfig {
    fn default() -> Self {
        Self {
            max_orphan_cache_size: default_max_orphan_cache_size(),
        }
    }
}
