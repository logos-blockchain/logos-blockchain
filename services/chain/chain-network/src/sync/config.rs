use std::num::NonZeroUsize;

#[derive(Debug, Clone)]
pub struct SyncConfig {
    pub orphan: OrphanConfig,
}

#[derive(Debug, Clone)]
pub struct OrphanConfig {
    /// The maximum number of pending orphans to keep in the cache.
    pub max_orphan_cache_size: NonZeroUsize,
}
