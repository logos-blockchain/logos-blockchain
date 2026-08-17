use std::hash::Hash;

use indexmap::IndexMap;

use crate::backend::{policy::TtlPolicy, pool::MempoolSettings};

/// Decides which valid transactions the pool stops carrying.
///
/// Composes eviction policies and drives their lifecycle; it never removes
/// anything itself — the pool executes the key lists it returns.
pub struct Evictor<Key> {
    ttl: TtlPolicy<Key>,
}

impl<Key> Evictor<Key>
where
    Key: Hash + Eq + Clone,
{
    #[must_use]
    pub fn new(settings: &MempoolSettings) -> Self {
        Self {
            ttl: TtlPolicy::new(settings.tx_ttl),
        }
    }

    pub fn on_add(&mut self, key: Key, now: u64) {
        self.ttl.on_add(key, now);
    }

    pub fn on_remove(&mut self, key: &Key) {
        self.ttl.on_remove(key);
    }

    /// Keys the pool should stop carrying, according to the enabled
    /// policies. The cause stays internal to the policies.
    #[must_use]
    pub fn select_evictions(&self, now: u64) -> Vec<Key> {
        self.ttl.expired(now)
    }

    #[must_use]
    pub fn save(&self) -> IndexMap<Key, u64> {
        self.ttl.save()
    }

    #[must_use]
    pub const fn recover(settings: &MempoolSettings, state: IndexMap<Key, u64>) -> Self {
        Self {
            ttl: TtlPolicy::recover(settings.tx_ttl, state),
        }
    }
}
