use std::{hash::Hash, time::Duration};

use indexmap::IndexMap;

/// Evicts transactions that stayed in the pool longer than the configured
/// TTL, regardless of pool fill.
///
/// Owns its own state: the insertion timestamp of every pending transaction.
/// Timestamps are tracked even when the TTL is disabled so that transaction
/// age survives recovery and a later configuration change.
pub struct TtlPolicy<Key> {
    tx_ttl: Option<Duration>,
    inserted: IndexMap<Key, u64>,
}

impl<Key> TtlPolicy<Key>
where
    Key: Hash + Eq + Clone,
{
    #[must_use]
    pub fn new(tx_ttl: Option<Duration>) -> Self {
        Self {
            tx_ttl,
            inserted: IndexMap::new(),
        }
    }

    pub fn on_add(&mut self, key: Key, now: u64) {
        self.inserted.insert(key, now);
    }

    pub fn on_remove(&mut self, key: &Key) {
        self.inserted.shift_remove(key);
    }

    /// Keys whose age exceeds the TTL.
    ///
    /// `inserted` is insertion-ordered with non-decreasing timestamps, so
    /// expired keys are always at the front and the scan stops at the first
    /// non-expired entry.
    #[must_use]
    pub fn expired(&self, now: u64) -> Vec<Key> {
        let Some(tx_ttl) = self.tx_ttl else {
            return Vec::new();
        };
        let ttl_millis = tx_ttl.as_millis() as u64;

        self.inserted
            .iter()
            .take_while(|(_, inserted_at)| now.saturating_sub(**inserted_at) >= ttl_millis)
            .map(|(key, _)| key.clone())
            .collect()
    }

    #[must_use]
    pub fn save(&self) -> IndexMap<Key, u64> {
        self.inserted.clone()
    }

    #[must_use]
    pub const fn recover(tx_ttl: Option<Duration>, state: IndexMap<Key, u64>) -> Self {
        Self {
            tx_ttl,
            inserted: state,
        }
    }
}
