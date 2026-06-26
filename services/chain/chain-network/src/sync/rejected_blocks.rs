use std::num::NonZeroUsize;

use lb_core::header::HeaderId;
use lru::LruCache;

use crate::metrics;

/// Bounded LRU of block IDs that the orphan pipeline should skip
/// (known-invalid or older-than-LIB).
pub struct RejectedBlocks {
    cache: LruCache<HeaderId, ()>,
}

impl RejectedBlocks {
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            cache: LruCache::new(capacity),
        }
    }

    /// Returns `true` if either the block itself or its parent (when known) is
    /// in the cache. Touches matching entries on hit so frequently-checked
    /// rejections stay in the cache.
    pub fn contains_block_or_parent(
        &mut self,
        block_id: &HeaderId,
        parent_id: Option<&HeaderId>,
    ) -> bool {
        self.cache.get(block_id).is_some() || parent_id.is_some_and(|p| self.cache.get(p).is_some())
    }

    pub fn insert(&mut self, block_id: HeaderId) {
        if self.cache.put(block_id, ()).is_none() {
            metrics::orphan_blocks_rejected_inserted_total();
        }
    }
}
