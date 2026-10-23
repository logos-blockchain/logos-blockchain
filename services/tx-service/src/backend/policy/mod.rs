pub mod powclaim;
pub mod ttl;

use std::collections::HashMap;

use indexmap::IndexMap;
use lb_core::mantle::{
    OpRef,
    ops::pow::ClaimPowRewardOp,
    traits::{Hashable, MantleTx},
};
use lb_cryptarchia_engine::Slot;
pub use powclaim::{PowClaimEvictionSettings, PowClaimPolicy};
pub use ttl::TtlPolicy;

use crate::backend::AdmissionTime;

/// The clock eviction policies see.
///
/// The pool has no clock of its own: wall-clock time is sampled on every
/// pool operation, and the slot only advances when the chain reports a
/// processed block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PolicyClock {
    /// Unix timestamp in milliseconds.
    pub now_millis: u64,
    /// Highest slot of any processed block, `None` until the first one.
    pub slot: Option<Slot>,
}

/// Slots of the blocks a transaction refers to, as far as the pool could
/// resolve them against storage before admitting the transaction.
pub type BlockSlots<BlockId> = HashMap<BlockId, Slot>;

/// One reason the pool stops carrying a valid transaction.
///
/// Policies own their state and see every transaction the pool admits or
/// retires; the evictor composes them and the pool executes what they
/// select. The admission time of each pending transaction is the pool's
/// state, which policies read in [`Self::select`].
pub trait EvictionPolicy<Tx, BlockId>: Send + Sync
where
    Tx: Hashable,
{
    /// Whether `tx` would be selected for eviction the moment it was added
    /// at `clock`, so the pool can refuse it instead.
    fn is_expired(
        &self,
        _tx: &Tx,
        _clock: &PolicyClock,
        _block_slots: &BlockSlots<BlockId>,
    ) -> bool {
        false
    }

    /// `tx` was admitted under `key` at `clock`. `block_slots` holds the
    /// slots of the blocks `tx` refers to that storage already knew.
    fn on_add(
        &mut self,
        key: &Tx::Hash,
        tx: &Tx,
        clock: &PolicyClock,
        block_slots: &BlockSlots<BlockId>,
    );

    /// `key` left the pool, whether included in a block or evicted.
    fn on_remove(&mut self, key: &Tx::Hash);

    /// The chain processed `block_id` at `slot`.
    fn on_processed_block(&mut self, _block_id: &BlockId, _slot: Slot) {}

    /// Keys this policy wants gone at `clock`, given the `pending_txs` keys
    /// mapped to their admission timestamp, oldest first.
    fn select(
        &self,
        clock: &PolicyClock,
        pending_txs: &IndexMap<Tx::Hash, AdmissionTime>,
    ) -> Vec<Tx::Hash>;
}

/// What eviction policies may inspect about a pool item.
pub trait EvictableTx {
    /// The `PoW` reward claims the transaction carries, in op order.
    fn pow_reward_claims(&self) -> impl Iterator<Item = &ClaimPowRewardOp>;
}

impl<T: MantleTx> EvictableTx for T {
    fn pow_reward_claims(&self) -> impl Iterator<Item = &ClaimPowRewardOp> {
        self.op_refs_iter().filter_map(|op| match op {
            OpRef::ClaimPowReward(claim) => Some(claim),
            _ => None,
        })
    }
}
