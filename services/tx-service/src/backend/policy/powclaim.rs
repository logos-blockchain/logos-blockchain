use std::{
    collections::{BTreeMap, HashMap},
    hash::Hash,
    num::NonZeroU64,
};

use indexmap::IndexSet;
use lb_core::mantle::traits::Hashable;
use lb_cryptarchia_engine::Slot;
use serde::{Deserialize, Serialize};

use crate::backend::policy::{
    BlockSlots, EvictableTx, EvictionPolicy, PolicyClock, PolicySnapshot,
};

/// Settings for [`PowClaimPolicy`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowClaimEvictionSettings {
    /// The consensus acceptance window, in slots: a claim anchored at slot
    /// `s` is accepted by blocks up to slot `s + slot_window` inclusive. Must
    /// match the ledger's `PoW` reward configuration.
    pub slot_window: NonZeroU64,
}

impl PowClaimEvictionSettings {
    /// How many slots past expiry a claim stays in the pool before it is
    /// evicted: twice the acceptance window, so a claim is only dropped once
    /// it is well beyond any reorg that could bring it back into the window.
    #[must_use]
    pub const fn grace_slots(&self) -> u64 {
        self.slot_window.get().saturating_mul(2)
    }
}

/// Evicts transactions carrying a `PoW` reward claim the ledger can no longer
/// accept.
///
/// A claim op names the block its puzzle was solved against but not that
/// block's slot, so expiry is resolved from the anchor block: the pool looks
/// its slot up in storage on admission, and claims whose anchor is not stored
/// yet wait for the chain to report it processed. A transaction with several
/// claims expires with its earliest one, since one rejected op rejects the
/// whole transaction.
///
/// The policy keeps no persistent state: it learns about transactions as the
/// pool admits them, so claims pending across a restart are left to the TTL.
pub struct PowClaimPolicy<Key, BlockId> {
    settings: PowClaimEvictionSettings,
    /// Eviction slot of every tracked transaction.
    deadline_by_key: HashMap<Key, Slot>,
    /// Tracked transactions ordered by eviction slot.
    keys_by_deadline: BTreeMap<Slot, IndexSet<Key>>,
    /// Transactions whose anchor block has not been seen yet, by anchor.
    parked_by_block: HashMap<BlockId, IndexSet<Key>>,
    /// The unseen anchors of every parked transaction.
    parked_by_key: HashMap<Key, IndexSet<BlockId>>,
}

impl<Key, BlockId> PowClaimPolicy<Key, BlockId>
where
    Key: Hash + Eq + Clone,
    BlockId: Hash + Eq + Clone,
{
    #[must_use]
    pub fn new(settings: PowClaimEvictionSettings) -> Self {
        Self {
            settings,
            deadline_by_key: HashMap::new(),
            keys_by_deadline: BTreeMap::new(),
            parked_by_block: HashMap::new(),
            parked_by_key: HashMap::new(),
        }
    }

    /// The first slot at which a claim anchored at `anchor_slot` is evicted:
    /// the slot after its acceptance window closes, plus the grace.
    fn deadline_for(&self, anchor_slot: Slot) -> Slot {
        u64::from(anchor_slot)
            .saturating_add(self.settings.slot_window.get())
            .saturating_add(1)
            .saturating_add(self.settings.grace_slots())
            .into()
    }

    /// Track `key` for eviction at `deadline`, or earlier if it already is.
    fn track(&mut self, key: Key, deadline: Slot) {
        if let Some(&current) = self.deadline_by_key.get(&key) {
            if current <= deadline {
                return;
            }
            self.untrack_deadline(&key, current);
        }
        self.deadline_by_key.insert(key.clone(), deadline);
        self.keys_by_deadline
            .entry(deadline)
            .or_default()
            .insert(key);
    }

    fn untrack_deadline(&mut self, key: &Key, deadline: Slot) {
        let Some(keys) = self.keys_by_deadline.get_mut(&deadline) else {
            return;
        };
        keys.swap_remove(key);
        if keys.is_empty() {
            self.keys_by_deadline.remove(&deadline);
        }
    }

    fn park(&mut self, key: Key, anchor: BlockId) {
        self.parked_by_block
            .entry(anchor.clone())
            .or_default()
            .insert(key.clone());
        self.parked_by_key.entry(key).or_default().insert(anchor);
    }

    fn unpark(&mut self, key: &Key) {
        let Some(anchors) = self.parked_by_key.remove(key) else {
            return;
        };
        for anchor in anchors {
            let Some(keys) = self.parked_by_block.get_mut(&anchor) else {
                continue;
            };
            keys.swap_remove(key);
            if keys.is_empty() {
                self.parked_by_block.remove(&anchor);
            }
        }
    }

    /// Transactions tracked for eviction, parked ones included.
    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.deadline_by_key.len()
            + self
                .parked_by_key
                .keys()
                .filter(|key| !self.deadline_by_key.contains_key(key))
                .count()
    }
}

impl<Tx, BlockId> EvictionPolicy<Tx, BlockId> for PowClaimPolicy<Tx::Hash, BlockId>
where
    Tx: Hashable + EvictableTx,
    Tx::Hash: Send + Sync,
    BlockId: Hash + Eq + Clone + From<[u8; 32]> + Send + Sync,
{
    /// A claim whose anchor is known and whose deadline the chain already
    /// reached. Claims with an unknown anchor are admitted and parked.
    fn is_expired(&self, tx: &Tx, clock: &PolicyClock, block_slots: &BlockSlots<BlockId>) -> bool {
        let Some(slot) = clock.slot else {
            return false;
        };
        tx.pow_reward_claims().any(|claim| {
            block_slots
                .get(&BlockId::from(claim.block_hash))
                .is_some_and(|&anchor_slot| self.deadline_for(anchor_slot) <= slot)
        })
    }

    fn on_add(
        &mut self,
        key: &Tx::Hash,
        tx: &Tx,
        _clock: &PolicyClock,
        block_slots: &BlockSlots<BlockId>,
    ) {
        for claim in tx.pow_reward_claims() {
            let anchor = BlockId::from(claim.block_hash);
            match block_slots.get(&anchor) {
                Some(&anchor_slot) => {
                    let deadline = self.deadline_for(anchor_slot);
                    self.track(key.clone(), deadline);
                }
                None => self.park(key.clone(), anchor),
            }
        }
    }

    fn on_remove(&mut self, key: &Tx::Hash) {
        self.unpark(key);
        if let Some(deadline) = self.deadline_by_key.remove(key) {
            self.untrack_deadline(key, deadline);
        }
    }

    fn on_processed_block(&mut self, block_id: &BlockId, slot: Slot) {
        let Some(keys) = self.parked_by_block.remove(block_id) else {
            return;
        };
        let deadline = self.deadline_for(slot);
        for key in keys {
            if let Some(anchors) = self.parked_by_key.get_mut(&key) {
                anchors.swap_remove(block_id);
                if anchors.is_empty() {
                    self.parked_by_key.remove(&key);
                }
            }
            self.track(key, deadline);
        }
    }

    fn select(&self, clock: &PolicyClock) -> Vec<Tx::Hash> {
        let Some(slot) = clock.slot else {
            return Vec::new();
        };
        self.keys_by_deadline
            .range(..=slot)
            .flat_map(|(_, keys)| keys.iter().cloned())
            .collect()
    }

    fn save(&self) -> PolicySnapshot<Tx::Hash> {
        PolicySnapshot::Stateless
    }
}

#[cfg(test)]
mod tests {
    use lb_core::{
        header::HeaderId,
        mantle::{
            Op,
            ops::{channel::inscribe::InscriptionOp, pow::ClaimPowRewardOp},
            transactions::{hash::TxHash, tx_list::ops::Ops},
        },
    };

    use super::*;

    type Policy = PowClaimPolicy<TxHash, HeaderId>;

    const NONE: [TxHash; 0] = [];

    const WINDOW: u64 = 10;

    /// With a window of 10 and the grace of twice that, a claim anchored at
    /// slot 100 is accepted through slot 110 and evicted from slot 131.
    fn policy() -> Policy {
        Policy::new(PowClaimEvictionSettings {
            slot_window: NonZeroU64::new(WINDOW).expect("non-zero"),
        })
    }

    fn claim(anchor: [u8; 32]) -> Op {
        Op::ClaimPowReward(ClaimPowRewardOp {
            block_hash: anchor,
            ..ClaimPowRewardOp::sample()
        })
    }

    fn clock(slot: Option<u64>) -> PolicyClock {
        PolicyClock {
            now_millis: 0,
            slot: slot.map(Slot::from),
        }
    }

    fn known(blocks: &[([u8; 32], u64)]) -> BlockSlots<HeaderId> {
        blocks
            .iter()
            .map(|(id, slot)| (HeaderId::from(*id), Slot::from(*slot)))
            .collect()
    }

    fn add(policy: &mut Policy, tx: &Ops, block_slots: &BlockSlots<HeaderId>) -> TxHash {
        let key = tx.hash();
        EvictionPolicy::<Ops, HeaderId>::on_add(policy, &key, tx, &clock(None), block_slots);
        key
    }

    fn select(policy: &Policy, slot: u64) -> Vec<TxHash> {
        EvictionPolicy::<Ops, HeaderId>::select(policy, &clock(Some(slot)))
    }

    #[test]
    fn claim_is_evicted_once_its_window_and_grace_pass() {
        let anchor = [1u8; 32];
        let mut policy = policy();
        let tx = Ops::from([claim(anchor)]);
        let key = add(&mut policy, &tx, &known(&[(anchor, 100)]));

        // Rejected by the ledger from slot 111, evicted once the grace passes.
        assert_eq!(select(&policy, 111), NONE);
        assert_eq!(select(&policy, 130), NONE);
        assert_eq!(select(&policy, 131), vec![key]);
    }

    #[test]
    fn claim_past_its_deadline_is_expired_on_arrival() {
        let anchor = [1u8; 32];
        let policy = policy();
        let tx = Ops::from([claim(anchor)]);
        let block_slots = known(&[(anchor, 100)]);
        let expired_at = |slot| {
            EvictionPolicy::<Ops, HeaderId>::is_expired(
                &policy,
                &tx,
                &clock(Some(slot)),
                &block_slots,
            )
        };

        assert!(!expired_at(130));
        assert!(expired_at(131));

        // Neither an unknown anchor nor a chain without blocks can expire it.
        assert!(!EvictionPolicy::<Ops, HeaderId>::is_expired(
            &policy,
            &tx,
            &clock(Some(u64::MAX)),
            &known(&[])
        ));
        assert!(!EvictionPolicy::<Ops, HeaderId>::is_expired(
            &policy,
            &tx,
            &clock(None),
            &block_slots
        ));
    }

    #[test]
    fn grace_is_twice_the_window() {
        let settings = PowClaimEvictionSettings {
            slot_window: NonZeroU64::new(7).expect("non-zero"),
        };
        assert_eq!(settings.grace_slots(), 14);

        let anchor = [1u8; 32];
        let mut policy = Policy::new(settings);
        let tx = Ops::from([claim(anchor)]);
        let key = add(&mut policy, &tx, &known(&[(anchor, 100)]));

        // Window closes after 107, grace runs to 121, eviction from 122.
        assert_eq!(select(&policy, 121), NONE);
        assert_eq!(select(&policy, 122), vec![key]);
    }

    #[test]
    fn nothing_is_selected_before_the_first_block() {
        let anchor = [1u8; 32];
        let mut policy = policy();
        let tx = Ops::from([claim(anchor)]);
        add(&mut policy, &tx, &known(&[(anchor, 0)]));

        assert_eq!(
            EvictionPolicy::<Ops, HeaderId>::select(&policy, &clock(None)),
            NONE
        );
    }

    #[test]
    fn transaction_without_claims_is_ignored() {
        let mut policy = policy();
        let tx = Ops::from([Op::ChannelInscribe(InscriptionOp::sample())]);
        add(&mut policy, &tx, &known(&[]));

        assert_eq!(policy.tracked(), 0);
        assert_eq!(select(&policy, u64::MAX), NONE);
    }

    #[test]
    fn earliest_claim_decides_the_deadline() {
        let early = [1u8; 32];
        let late = [2u8; 32];
        let mut policy = policy();
        let tx = Ops::from([claim(late), claim(early)]);
        let key = add(&mut policy, &tx, &known(&[(early, 100), (late, 200)]));

        assert_eq!(select(&policy, 131), vec![key]);
        assert_eq!(policy.tracked(), 1);
    }

    #[test]
    fn unknown_anchor_waits_for_its_block() {
        let anchor = [1u8; 32];
        let mut policy = policy();
        let tx = Ops::from([claim(anchor)]);
        let key = add(&mut policy, &tx, &known(&[]));

        assert_eq!(select(&policy, u64::MAX), NONE);
        assert_eq!(policy.tracked(), 1);

        EvictionPolicy::<Ops, HeaderId>::on_processed_block(
            &mut policy,
            &HeaderId::from(anchor),
            Slot::from(100u64),
        );
        assert_eq!(select(&policy, 130), NONE);
        assert_eq!(select(&policy, 131), vec![key]);
        assert!(policy.parked_by_key.is_empty());
        assert!(policy.parked_by_block.is_empty());
    }

    #[test]
    fn late_anchor_can_only_bring_the_deadline_forward() {
        let known_anchor = [1u8; 32];
        let unknown_anchor = [2u8; 32];
        let mut policy = policy();
        let tx = Ops::from([claim(known_anchor), claim(unknown_anchor)]);
        let key = add(&mut policy, &tx, &known(&[(known_anchor, 200)]));

        EvictionPolicy::<Ops, HeaderId>::on_processed_block(
            &mut policy,
            &HeaderId::from(unknown_anchor),
            Slot::from(100u64),
        );
        assert_eq!(select(&policy, 131), vec![key]);

        // A later anchor than the one already tracked changes nothing.
        let mut policy = policy_with(&[(known_anchor, 100)], &tx);
        EvictionPolicy::<Ops, HeaderId>::on_processed_block(
            &mut policy,
            &HeaderId::from(unknown_anchor),
            Slot::from(200u64),
        );
        assert_eq!(select(&policy, 131), vec![key]);
        assert_eq!(policy.tracked(), 1);
    }

    fn policy_with(blocks: &[([u8; 32], u64)], tx: &Ops) -> Policy {
        let mut policy = policy();
        add(&mut policy, tx, &known(blocks));
        policy
    }

    #[test]
    fn removal_forgets_tracked_and_parked_keys() {
        let known_anchor = [1u8; 32];
        let unknown_anchor = [2u8; 32];
        let mut policy = policy();
        let tracked = Ops::from([claim(known_anchor)]);
        let parked = Ops::from([claim(unknown_anchor)]);
        let tracked_key = add(&mut policy, &tracked, &known(&[(known_anchor, 100)]));
        let parked_key = add(&mut policy, &parked, &known(&[]));
        assert_eq!(policy.tracked(), 2);

        EvictionPolicy::<Ops, HeaderId>::on_remove(&mut policy, &tracked_key);
        EvictionPolicy::<Ops, HeaderId>::on_remove(&mut policy, &parked_key);

        assert_eq!(policy.tracked(), 0);
        assert!(policy.keys_by_deadline.is_empty());
        assert!(policy.parked_by_block.is_empty());

        // The parked anchor arriving later finds nothing to resolve.
        EvictionPolicy::<Ops, HeaderId>::on_processed_block(
            &mut policy,
            &HeaderId::from(unknown_anchor),
            Slot::from(0u64),
        );
        assert_eq!(select(&policy, u64::MAX), NONE);
    }
}
