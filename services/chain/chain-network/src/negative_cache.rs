//! In-memory negative cache of block ids that belong to dead forks.
//!
//! A "dead fork" is a branch that diverges below the last immutable block
//! (LIB) and therefore can never become part of the canonical chain. During
//! the 2026-06-24 incident such forks were re-gossiped endlessly: each block
//! was reconstructed, re-applied (failing with `ParentMissing`), and
//! re-enqueued for orphan download, amplifying into ~20GB/h of logs.
//!
//! The chain-network service consults this cache as blocks arrive so that
//! dead-fork blocks (and their descendants) are dropped immediately instead of
//! being reprocessed.

use std::collections::{HashSet, VecDeque};

use lb_core::header::HeaderId;

/// A bounded, FIFO-evicting set of block ids known to be on dead forks.
pub struct NegativeCache {
    ids: HashSet<HeaderId>,
    order: VecDeque<HeaderId>,
    capacity: usize,
}

impl NegativeCache {
    /// Creates a cache holding at most `capacity` entries.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            ids: HashSet::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Returns whether `id` is currently in the cache.
    #[must_use]
    pub fn contains(&self, id: &HeaderId) -> bool {
        self.ids.contains(id)
    }

    /// Inserts `id`, evicting the oldest entry if the cache is at capacity.
    pub fn insert(&mut self, id: HeaderId) {
        if self.ids.insert(id) {
            self.order.push_back(id);
            if self.order.len() > self.capacity
                && let Some(oldest) = self.order.pop_front()
            {
                self.ids.remove(&oldest);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> HeaderId {
        [n; 32].into()
    }

    #[test]
    fn contains_after_insert() {
        let mut cache = NegativeCache::new(8);
        assert!(!cache.contains(&id(1)));
        cache.insert(id(1));
        assert!(cache.contains(&id(1)));
    }

    #[test]
    fn evicts_oldest_when_full() {
        let mut cache = NegativeCache::new(2);
        cache.insert(id(1));
        cache.insert(id(2));
        cache.insert(id(3)); // evicts id(1)

        assert!(!cache.contains(&id(1)));
        assert!(cache.contains(&id(2)));
        assert!(cache.contains(&id(3)));
    }

    #[test]
    fn duplicate_insert_does_not_grow_or_reorder() {
        let mut cache = NegativeCache::new(2);
        cache.insert(id(1));
        cache.insert(id(1)); // no-op
        cache.insert(id(2));
        cache.insert(id(3)); // evicts id(1), not id(2)

        assert!(!cache.contains(&id(1)));
        assert!(cache.contains(&id(2)));
        assert!(cache.contains(&id(3)));
    }
}
