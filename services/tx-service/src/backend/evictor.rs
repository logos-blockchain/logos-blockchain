use std::hash::Hash;

use indexmap::{IndexMap, IndexSet};
use lb_core::mantle::traits::Hashable;
use lb_cryptarchia_engine::Slot;

use crate::backend::{
    AdmissionTime,
    policy::{BlockSlots, EvictableTx, EvictionPolicy, PolicyClock, PowClaimPolicy, TtlPolicy},
    pool::MempoolSettings,
};

/// Decides which valid transactions the pool stops carrying.
///
/// Composes eviction policies and drives their lifecycle; it never removes
/// anything itself — the pool executes the key lists it returns.
pub struct Evictor<Tx, BlockId>
where
    Tx: Hashable,
{
    policies: Vec<Box<dyn EvictionPolicy<Tx, BlockId>>>,
    /// Highest slot of any processed block, `None` until the first one.
    slot: Option<Slot>,
}

impl<Tx, BlockId> Evictor<Tx, BlockId>
where
    Tx: Hashable + EvictableTx + 'static,
    Tx::Hash: Hash + Eq + Clone + Send + Sync + 'static,
    BlockId: Hash + Eq + Clone + From<[u8; 32]> + Send + Sync + 'static,
{
    #[must_use]
    pub fn new(settings: &MempoolSettings) -> Self {
        let mut policies: Vec<Box<dyn EvictionPolicy<Tx, BlockId>>> =
            vec![Box::new(TtlPolicy::new(settings.tx_ttl))];
        if let Some(pow_claim) = &settings.pow_claim {
            policies.push(Box::new(PowClaimPolicy::new(pow_claim.clone())));
        }
        Self {
            policies,
            slot: None,
        }
    }

    /// The clock at wall-clock time `now_millis`.
    const fn clock(&self, now_millis: u64) -> PolicyClock {
        PolicyClock {
            now_millis,
            slot: self.slot,
        }
    }

    /// Whether some policy would evict `tx` the moment it was added.
    #[must_use]
    pub fn is_expired(&self, tx: &Tx, now_millis: u64, block_slots: &BlockSlots<BlockId>) -> bool {
        let clock = self.clock(now_millis);
        self.policies
            .iter()
            .any(|policy| policy.is_expired(tx, &clock, block_slots))
    }

    pub fn on_add(
        &mut self,
        key: &Tx::Hash,
        tx: &Tx,
        now_millis: u64,
        block_slots: &BlockSlots<BlockId>,
    ) {
        let clock = self.clock(now_millis);
        for policy in &mut self.policies {
            policy.on_add(key, tx, &clock, block_slots);
        }
    }

    pub fn on_remove(&mut self, key: &Tx::Hash) {
        for policy in &mut self.policies {
            policy.on_remove(key);
        }
    }

    /// The chain processed `block_id` at `slot`. The slot never moves
    /// backwards: blocks off the canonical chain still count, since a block's
    /// slot does not depend on the chain it ends up on.
    pub fn on_processed_block(&mut self, block_id: &BlockId, slot: Slot) {
        self.slot = Some(self.slot.map_or(slot, |current| current.max(slot)));
        for policy in &mut self.policies {
            policy.on_processed_block(block_id, slot);
        }
    }

    /// Keys the pool should stop carrying at wall-clock time `now_millis`,
    /// among the `pending_txs` keys mapped to their admission timestamp,
    /// according to the enabled policies. The cause stays internal to the
    /// policies; a key wanted by several of them is returned once.
    #[must_use]
    pub fn select_evictions(
        &self,
        now_millis: u64,
        pending_txs: &IndexMap<Tx::Hash, AdmissionTime>,
    ) -> Vec<Tx::Hash> {
        let clock = self.clock(now_millis);
        self.policies
            .iter()
            .flat_map(|policy| policy.select(&clock, pending_txs))
            .collect::<IndexSet<_>>()
            .into_iter()
            .collect()
    }
}
