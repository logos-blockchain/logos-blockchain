use std::{
    collections::{BTreeMap, HashMap},
    hash::Hash,
    time::Duration,
};

use indexmap::IndexMap;
use lb_core::mantle::traits::Hashable;

use crate::backend::{
    AdmissionTime, DEFAULT_TX_TTL, IncludedItem,
    policy::{BlockSlots, EvictionPolicy, PolicyClock},
};

/// How long a retired transaction no canonical block carries stays resolvable
/// after its age reaches the TTL.
const RETIREMENT_GRACE: Duration = Duration::from_mins(10);

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

    /// Keys in `retired_txs` to release: those whose age reached the TTL plus
    /// `RETIREMENT_GRACE`, so a retired transaction stays resolvable for as
    /// long as a leader may still select it, wherever it was retired. A node
    /// that disables expiry counts the default TTL.
    // TODO: RFC should define how to deal with TTL=None: https://github.com/logos-co/logos-lips/pull/448/changes#r4450950951
    #[must_use]
    pub fn due_for_release<Key>(
        &self,
        retired_txs: &BTreeMap<Key, AdmissionTime>,
        now: u64,
    ) -> Vec<Key>
    where
        Key: Clone,
    {
        let release_age = self.tx_ttl.unwrap_or(DEFAULT_TX_TTL) + RETIREMENT_GRACE;
        let release_age_millis = release_age.as_millis() as u64;

        retired_txs
            .iter()
            .filter(|(_, admitted_at)| now.saturating_sub(**admitted_at) >= release_age_millis)
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

    fn on_release(&mut self, _key: &Tx::Hash) {}

    fn select_retirements(
        &self,
        clock: &PolicyClock,
        pending_txs: &IndexMap<Tx::Hash, AdmissionTime>,
    ) -> Vec<Tx::Hash> {
        self.expired(pending_txs, clock.now_millis)
    }

    fn select_releases(
        &self,
        clock: &PolicyClock,
        retired_txs: &BTreeMap<Tx::Hash, AdmissionTime>,
        _included_txs: &HashMap<Tx::Hash, IncludedItem>,
    ) -> Vec<Tx::Hash> {
        self.due_for_release(retired_txs, clock.now_millis)
    }
}
