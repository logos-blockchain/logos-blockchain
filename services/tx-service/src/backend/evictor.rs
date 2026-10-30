use std::hash::Hash;

use indexmap::{IndexMap, IndexSet};
use lb_core::mantle::traits::Hashable;
use lb_cryptarchia_engine::Slot;

use crate::backend::{
    policy::{
        BlockSlots, EvictableTx, EvictionPolicy, PolicyClock, PolicySnapshot, PowClaimPolicy,
        TtlPolicy,
    },
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
        Self::with_ttl(settings, TtlPolicy::new(settings.tx_ttl))
    }

    #[must_use]
    pub fn recover(settings: &MempoolSettings, state: IndexMap<Tx::Hash, u64>) -> Self {
        Self::with_ttl(settings, TtlPolicy::recover(settings.tx_ttl, state))
    }

    fn with_ttl(settings: &MempoolSettings, ttl: TtlPolicy<Tx::Hash>) -> Self {
        let mut policies: Vec<Box<dyn EvictionPolicy<Tx, BlockId>>> = vec![Box::new(ttl)];
        if let Some(pow_claim) = settings.pow_claim {
            policies.push(Box::new(PowClaimPolicy::new(pow_claim)));
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
    /// according to the enabled policies. The cause stays internal to the
    /// policies; a key wanted by several of them is returned once.
    #[must_use]
    pub fn select_evictions(&self, now_millis: u64) -> Vec<Tx::Hash> {
        let clock = self.clock(now_millis);
        self.policies
            .iter()
            .flat_map(|policy| policy.select(&clock))
            .collect::<IndexSet<_>>()
            .into_iter()
            .collect()
    }

    /// Insertion timestamp of every pending transaction, as the TTL policy
    /// tracks it. This doubles as the pool's pending set on recovery.
    #[must_use]
    pub fn save(&self) -> IndexMap<Tx::Hash, u64> {
        self.policies
            .iter()
            .find_map(|policy| match policy.save() {
                PolicySnapshot::Ttl(inserted) => Some(inserted),
                PolicySnapshot::Stateless => None,
            })
            .unwrap_or_default()
    }
}
