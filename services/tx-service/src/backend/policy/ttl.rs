use std::{hash::Hash, time::Duration};

use indexmap::IndexMap;
use lb_core::mantle::traits::Hashable;

use crate::backend::{
    AdmissionTime,
    policy::{BlockSlots, EvictionPolicy, PolicyClock},
};

/// Evicts transactions that stayed in the pool longer than the configured
/// TTL, regardless of pool fill.
///
/// Holds no state: a transaction's age is read from the admission time the
/// pool keeps for it.
pub struct TtlPolicy {
    tx_ttl: Option<Duration>,
}

impl TtlPolicy {
    #[must_use]
    pub fn new(tx_ttl: impl Into<Option<Duration>>) -> Self {
        Self {
            tx_ttl: tx_ttl.into(),
        }
    }

    /// Keys in `pending_txs` whose age exceeds the TTL.
    ///
    /// `pending_txs` is admission-ordered with non-decreasing timestamps, so
    /// expired keys are always at the front and the scan stops at the first
    /// non-expired entry.
    #[must_use]
    pub fn expired<Key>(&self, pending_txs: &IndexMap<Key, AdmissionTime>, now: u64) -> Vec<Key>
    where
        Key: Hash + Eq + Clone,
    {
        let Some(tx_ttl) = self.tx_ttl else {
            return Vec::new();
        };
        let ttl_millis = tx_ttl.as_millis() as u64;

        pending_txs
            .iter()
            .take_while(|(_, admitted_at)| now.saturating_sub(**admitted_at) >= ttl_millis)
            .map(|(key, _)| key.clone())
            .collect()
    }
}

impl<Tx, BlockId> EvictionPolicy<Tx, BlockId> for TtlPolicy
where
    Tx: Hashable,
    Tx::Hash: Hash + Eq + Clone,
{
    // Admission times are the pool's state, so there is nothing to track.
    fn on_add(
        &mut self,
        _key: &Tx::Hash,
        _tx: &Tx,
        _clock: &PolicyClock,
        _block_slots: &BlockSlots<BlockId>,
    ) {
    }

    fn on_remove(&mut self, _key: &Tx::Hash) {}

    fn select(
        &self,
        clock: &PolicyClock,
        pending_txs: &IndexMap<Tx::Hash, AdmissionTime>,
    ) -> Vec<Tx::Hash> {
        self.expired(pending_txs, clock.now_millis)
    }
}
