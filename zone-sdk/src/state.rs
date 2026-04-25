use std::collections::{BTreeMap, HashMap};

use lb_core::{
    header::HeaderId,
    mantle::{SignedMantleTx, Transaction as _, ops::channel::MsgId, tx::TxHash},
};
use rpds::HashTrieSetSync;

/// Channel inscription observed in an L1 block.
#[derive(Debug, Clone)]
pub struct InscriptionInfo {
    /// The transaction hash containing this inscription.
    pub tx_hash: TxHash,
    /// The parent message ID this inscription chains from.
    pub parent_msg: MsgId,
    /// The message ID of this inscription.
    pub this_msg: MsgId,
    /// The opaque inscription payload.
    pub payload: Vec<u8>,
}

/// Result of channel update detection — the linear block-level delta
/// between two canonical chains.
///
/// - `orphaned`: inscriptions on blocks of the old canonical chain that are not
///   on blocks of the new canonical chain. Revert from state.
/// - `adopted`: inscriptions on blocks of the new canonical chain that are not
///   on blocks of the old canonical chain. Apply to state.
/// - When `orphaned` is empty, this is an extension-only update.
#[derive(Debug)]
pub struct ChannelUpdateInfo {
    /// Inscriptions removed from the canonical chain (revert from state).
    pub orphaned: Vec<InscriptionInfo>,
    /// Inscriptions added to the canonical chain (apply to state).
    pub adopted: Vec<InscriptionInfo>,
    /// The new channel tip `MsgId`.
    pub new_channel_tip: MsgId,
}

impl ChannelUpdateInfo {
    /// Returns true if this update orphaned pending inscriptions,
    /// meaning a competing inscription or L1 reorg broke our pending chain.
    #[must_use]
    pub const fn is_conflict(&self) -> bool {
        !self.orphaned.is_empty()
    }
}

/// Local pending inscription with lineage metadata.
#[derive(Debug, Clone)]
pub struct PendingInscription {
    pub tx_hash: TxHash,
    pub signed_tx: SignedMantleTx,
    pub parent_msg: MsgId,
    pub this_msg: MsgId,
    pub payload: Vec<u8>,
}

/// Transaction state tracker.
pub struct TxState {
    /// Local pending inscriptions indexed by tx hash.
    pending: HashMap<TxHash, PendingInscription>,
    /// Reverse index: parent `MsgId` → tx hashes that chain from it.
    pending_by_parent: HashMap<MsgId, Vec<TxHash>>,
    /// Non-inscription pending txs (e.g. `set_keys`).
    pending_other: HashMap<TxHash, SignedMantleTx>,
    /// Per-block cumulative safe sets.
    block_states: BTreeMap<HeaderId, HashTrieSetSync<TxHash>>,
    /// Block parent relationships for pruning.
    parent_map: HashMap<HeaderId, HeaderId>,
    /// Current LIB for pruning.
    current_lib: HeaderId,
    /// Channel inscriptions per L1 block (unfinalized window only).
    block_inscriptions: HashMap<HeaderId, Vec<InscriptionInfo>>,
    /// Last finalized channel tip — used as parent when pending is empty.
    finalized_msg: MsgId,
}

impl TxState {
    #[must_use]
    pub fn new(lib: HeaderId, finalized_msg: MsgId) -> Self {
        let mut block_states = BTreeMap::new();
        block_states.insert(lib, HashTrieSetSync::new_sync());
        Self {
            pending: HashMap::new(),
            pending_by_parent: HashMap::new(),
            pending_other: HashMap::new(),
            block_states,
            parent_map: HashMap::new(),
            current_lib: lib,
            block_inscriptions: HashMap::new(),
            finalized_msg,
        }
    }

    /// Last finalized channel tip `MsgId`.
    #[must_use]
    pub const fn finalized_msg(&self) -> MsgId {
        self.finalized_msg
    }

    /// Update the finalized channel tip from backfilled finalized history.
    pub const fn set_finalized_msg(&mut self, msg: MsgId) {
        self.finalized_msg = msg;
    }

    /// Submit an inscription tx for tracking with lineage metadata.
    pub fn submit_inscription(
        &mut self,
        signed_tx: SignedMantleTx,
        parent_msg: MsgId,
        this_msg: MsgId,
        payload: Vec<u8>,
    ) {
        let tx_hash = signed_tx.mantle_tx.hash();
        self.pending_by_parent
            .entry(parent_msg)
            .or_default()
            .push(tx_hash);
        self.pending.insert(
            tx_hash,
            PendingInscription {
                tx_hash,
                signed_tx,
                parent_msg,
                this_msg,
                payload,
            },
        );
    }

    /// Submit a non-inscription tx for tracking (e.g. `set_keys`).
    pub fn submit_other(&mut self, signed_tx: SignedMantleTx) {
        let tx_hash = signed_tx.mantle_tx.hash();
        self.pending_other.insert(tx_hash, signed_tx);
    }

    /// Process a new block. Finalization is handled by backfill ground
    /// truth, not by the safe-set walk here.
    pub fn process_block(
        &mut self,
        block_id: HeaderId,
        parent_id: HeaderId,
        lib: HeaderId,
        our_txs: impl IntoIterator<Item = TxHash>,
        inscriptions: Vec<InscriptionInfo>,
    ) {
        // Store parent relationship for pruning
        self.parent_map.insert(block_id, parent_id);

        // Build cumulative safe set from parent. Parent may be missing
        // when blocks are processed from slot-range backfill and LIB has
        // advanced between batches (pruning the parent). Starting with an
        // empty set is conservative: txs show as "pending" until seen in
        // a subsequent block with a known parent.
        let mut safe_set = self
            .block_states
            .get(&parent_id)
            .cloned()
            .unwrap_or_default();

        for tx in our_txs {
            if self.pending.contains_key(&tx) || self.pending_other.contains_key(&tx) {
                safe_set = safe_set.insert(tx);
            }
        }
        self.block_states.insert(block_id, safe_set);

        // Store channel inscriptions for this block
        if !inscriptions.is_empty() {
            self.block_inscriptions.insert(block_id, inscriptions);
        }

        // When lib advances: update finalized_msg and prune.
        // NOTE: we do NOT remove pending txs here. Pending txs are only
        // removed when confirmed by backfill ground truth (canonical
        // finalized blocks from the node). The safe set is used for
        // branch-relative status (pending_txs resubmission) but not
        // as proof of canonical finalization — it can include blocks
        // from orphaned branches in concurrent scenarios.
        if lib != self.current_lib {
            // Compute finalized_msg BEFORE pruning — walk from new LIB
            // backwards to find the latest inscription in the finalized range.
            self.finalized_msg = self.channel_tip_at(lib);

            // Prune ancestors of new lib (but not lib itself)
            let mut prune_cursor = self.parent_map.get(&lib).copied();
            while let Some(b) = prune_cursor {
                self.block_states.remove(&b);
                self.block_inscriptions.remove(&b);
                prune_cursor = self.parent_map.remove(&b);
            }

            // Remove finalized tx hashes from all safe sets. Using remove
            // (rather than rebuild) preserves rpds memory sharing between
            // block states for non-finalized txs.
            if let Some(lib_safe_set) = self.block_states.get(&lib) {
                let finalized_hashes: Vec<TxHash> = lib_safe_set
                    .iter()
                    .filter(|hash| {
                        !self.pending.contains_key(hash) && !self.pending_other.contains_key(hash)
                    })
                    .copied()
                    .collect();
                for safe_set in self.block_states.values_mut() {
                    for tx_hash in &finalized_hashes {
                        *safe_set = safe_set.remove(tx_hash);
                    }
                }
            }

            self.prune_orphans(lib);
            self.current_lib = lib;
        }
    }

    /// Remove orphaned blocks whose parent was pruned.
    fn prune_orphans(&mut self, lib: HeaderId) {
        loop {
            let orphans: Vec<_> = self
                .parent_map
                .iter()
                .filter_map(|(id, parent)| {
                    if *id == lib {
                        return None; // lib is root
                    }
                    let parent_is_lib = *parent == lib;
                    let parent_exists = self.parent_map.contains_key(parent);
                    (!parent_is_lib && !parent_exists).then_some(*id)
                })
                .collect();

            if orphans.is_empty() {
                break;
            }

            for orphan in orphans {
                self.block_states.remove(&orphan);
                self.block_inscriptions.remove(&orphan);
                self.parent_map.remove(&orphan);
            }
        }
    }

    /// Pending txs eligible for resubmission: not yet safe at tip AND
    /// part of the local suffix reachable from canonical channel tip.
    ///
    /// Returned in parent-before-child order (BFS from channel tip via
    /// `pending_by_parent`) so the node's mempool sees the parent before
    /// any child — matters on checkpoint resume, where `HashMap`
    /// iteration order is arbitrary.
    pub fn pending_txs(&self, tip: HeaderId) -> Vec<(TxHash, SignedMantleTx)> {
        let safe = self
            .block_states
            .get(&tip)
            .cloned()
            .unwrap_or_else(HashTrieSetSync::new_sync);

        let channel_tip = self.channel_tip_at(tip);
        let inscriptions = self
            .collect_pending_suffix(channel_tip)
            .into_iter()
            .filter(|inv| !safe.contains(&inv.tx_hash))
            .filter_map(|inv| {
                self.pending
                    .get(&inv.tx_hash)
                    .map(|p| (inv.tx_hash, p.signed_tx.clone()))
            });
        let others = self
            .pending_other
            .iter()
            .filter(|(hash, _)| !safe.contains(hash))
            .map(|(hash, tx)| (*hash, tx.clone()));
        inscriptions.chain(others).collect()
    }

    /// Number of pending transactions (all types).
    #[must_use]
    pub fn unfinalized_count(&self) -> usize {
        self.pending.len() + self.pending_other.len()
    }

    /// Whether there are pending channel inscriptions.
    #[must_use]
    pub fn has_pending_inscriptions(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Pending inscriptions valid to be added at the current channel tip:
    /// inscriptions in `self.pending` that chain transitively from the
    /// channel tip at `tip`.
    ///
    /// Excludes pending inscriptions already included in a block — those are
    /// reported via `adopted` in [`ChannelUpdateInfo`] instead.
    #[must_use]
    pub fn pending_on_branch(&self, tip: HeaderId) -> Vec<InscriptionInfo> {
        let channel_tip = self.channel_tip_at(tip);
        self.collect_pending_suffix(channel_tip)
    }

    /// Remove pending inscriptions whose lineage does NOT reach the current
    /// channel tip and that aren't already in a block on this branch.
    /// Returns the removed entries in **parent-before-child (BFS) order** so
    /// a consumer that iterates and republishes naturally rebuilds the chain
    /// in dependency order. Keeps `self.pending` linear.
    pub fn shed_off_branch_pending(&mut self, tip: HeaderId) -> Vec<InscriptionInfo> {
        if self.pending.is_empty() {
            return Vec::new();
        }
        let channel_tip = self.channel_tip_at(tip);
        let on_branch: std::collections::HashSet<TxHash> = self
            .collect_pending_suffix(channel_tip)
            .iter()
            .map(|i| i.tx_hash)
            .collect();
        let safe: std::collections::HashSet<TxHash> = self
            .block_states
            .get(&tip)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();

        let eligible: std::collections::HashSet<TxHash> = self
            .pending
            .keys()
            .filter(|h| !on_branch.contains(h) && !safe.contains(h))
            .copied()
            .collect();
        if eligible.is_empty() {
            return Vec::new();
        }

        // Find root parents: parent_msg values for eligible entries whose
        // parent is NOT the `this_msg` of another eligible entry. Sort for
        // determinism across HashMap iteration order.
        let eligible_this_msgs: std::collections::HashSet<MsgId> = eligible
            .iter()
            .filter_map(|h| self.pending.get(h).map(|p| p.this_msg))
            .collect();
        let mut root_parents: Vec<MsgId> = eligible
            .iter()
            .filter_map(|h| {
                let p = self.pending.get(h)?;
                if eligible_this_msgs.contains(&p.parent_msg) {
                    None
                } else {
                    Some(p.parent_msg)
                }
            })
            .collect();
        root_parents.sort_by_key(|m| <[u8; 32]>::from(*m));
        root_parents.dedup();

        // BFS from each root parent via pending_by_parent; collect only
        // eligible entries in parent-first order.
        let mut ordered = Vec::with_capacity(eligible.len());
        let mut seen = std::collections::HashSet::new();
        for root in root_parents {
            for inv in self.collect_pending_suffix(root) {
                if eligible.contains(&inv.tx_hash) && seen.insert(inv.tx_hash) {
                    ordered.push(inv);
                }
            }
        }

        for inv in &ordered {
            self.remove_pending(&inv.tx_hash);
        }
        ordered
    }

    /// Check if we have state for a block.
    #[must_use]
    pub fn has_block(&self, block_id: &HeaderId) -> bool {
        self.block_states.contains_key(block_id)
    }

    /// Current LIB.
    #[must_use]
    pub const fn lib(&self) -> HeaderId {
        self.current_lib
    }

    /// All pending transactions (for checkpoint serialization).
    #[must_use]
    pub fn all_pending_txs(&self) -> Vec<(TxHash, SignedMantleTx)> {
        let inscriptions = self
            .pending
            .iter()
            .map(|(hash, p)| (*hash, p.signed_tx.clone()));
        let others = self
            .pending_other
            .iter()
            .map(|(hash, tx)| (*hash, tx.clone()));
        inscriptions.chain(others).collect()
    }

    /// Remove a pending inscription and return its signed tx.
    pub fn remove_pending(&mut self, tx_hash: &TxHash) -> Option<SignedMantleTx> {
        if let Some(removed) = self.pending.remove(tx_hash) {
            if let Some(children) = self.pending_by_parent.get_mut(&removed.parent_msg) {
                children.retain(|h| h != tx_hash);
                if children.is_empty() {
                    self.pending_by_parent.remove(&removed.parent_msg);
                }
            }
            Some(removed.signed_tx)
        } else {
            self.pending_other.remove(tx_hash)
        }
    }

    /// Derive the publish parent from state.
    ///
    /// Walks the local pending suffix from canonical tip only if the
    /// lineage is unambiguous (exactly one child at each step).
    /// Falls back to canonical tip if ambiguous or no pending suffix.
    #[must_use]
    pub fn publish_parent(&self, tip: HeaderId) -> MsgId {
        let channel_tip = self.channel_tip_at(tip);
        self.pending_publish_tail(channel_tip)
            .unwrap_or(channel_tip)
    }

    /// Walk local pending lineage from `from_msg` to find the tail,
    /// but ONLY if the chain is strictly linear (one child per parent).
    /// Returns None if no pending children or if lineage branches.
    fn pending_publish_tail(&self, from_msg: MsgId) -> Option<MsgId> {
        let mut current = from_msg;
        let mut found_any = false;

        loop {
            let Some(children) = self.pending_by_parent.get(&current) else {
                return found_any.then_some(current);
            };
            if children.len() != 1 {
                return found_any.then_some(current);
            }
            let Some(pending) = self.pending.get(&children[0]) else {
                return found_any.then_some(current);
            };
            current = pending.this_msg;
            found_any = true;
        }
    }

    /// Derive the channel tip `MsgId` at a given L1 block by walking backwards
    /// through the block tree and finding the most recent inscription.
    /// Returns `finalized_msg` if no inscriptions are found in the
    /// unfinalized window.
    #[must_use]
    pub fn channel_tip_at(&self, block_id: HeaderId) -> MsgId {
        let mut current = block_id;
        loop {
            if let Some(inscs) = self.block_inscriptions.get(&current)
                && let Some(last) = inscs.last()
            {
                return last.this_msg;
            }

            if current == self.current_lib {
                return self.finalized_msg;
            }

            match self.parent_map.get(&current) {
                Some(&parent) => current = parent,
                None => return self.finalized_msg,
            }
        }
    }

    /// Detect a channel update between old and new L1 tips.
    ///
    /// Returns the linear block-level delta between the two canonical chains:
    /// - `orphaned`: inscriptions on blocks of the old canonical chain that are
    ///   not on blocks of the new canonical chain (revert from state).
    /// - `adopted`: inscriptions on blocks of the new canonical chain that are
    ///   not on blocks of the old canonical chain (apply to state).
    ///
    /// Returns `None` if no channel state change.
    #[must_use]
    pub fn detect_channel_update(
        &self,
        old_tip: HeaderId,
        new_tip: HeaderId,
    ) -> Option<ChannelUpdateInfo> {
        let old_channel_tip = self.channel_tip_at(old_tip);
        let new_channel_tip = self.channel_tip_at(new_tip);

        if old_channel_tip == new_channel_tip {
            return None;
        }

        let new_branch = self.collect_inscriptions_on_branch(new_tip);
        let old_branch = self.collect_inscriptions_on_branch(old_tip);

        let new_chain: std::collections::HashSet<MsgId> =
            new_branch.iter().map(|i| i.this_msg).collect();
        let old_chain: std::collections::HashSet<MsgId> =
            old_branch.iter().map(|i| i.this_msg).collect();

        let adopted: Vec<InscriptionInfo> = new_branch
            .iter()
            .filter(|i| !old_chain.contains(&i.this_msg))
            .cloned()
            .collect();

        let orphaned: Vec<InscriptionInfo> = old_branch
            .into_iter()
            .filter(|i| !new_chain.contains(&i.this_msg))
            .collect();

        if orphaned.is_empty() && adopted.is_empty() {
            return None;
        }

        Some(ChannelUpdateInfo {
            orphaned,
            adopted,
            new_channel_tip,
        })
    }

    /// Collect ALL pending inscriptions reachable from `from_msg`.
    /// Uses the `pending_by_parent` index. Handles branching (multiple
    /// children per parent) by collecting all branches.
    /// Returns inscriptions in BFS order (parents before children).
    pub(crate) fn collect_pending_suffix(&self, from_msg: MsgId) -> Vec<InscriptionInfo> {
        let mut suffix = Vec::new();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(from_msg);

        while let Some(current) = queue.pop_front() {
            let Some(children) = self.pending_by_parent.get(&current) else {
                continue;
            };
            for child_hash in children {
                let Some(pending) = self.pending.get(child_hash) else {
                    continue;
                };
                suffix.push(InscriptionInfo {
                    tx_hash: pending.tx_hash,
                    parent_msg: pending.parent_msg,
                    this_msg: pending.this_msg,
                    payload: pending.payload.clone(),
                });
                queue.push_back(pending.this_msg);
            }
        }

        suffix
    }

    /// Collect all inscriptions on a branch from the given block back to LIB,
    /// in oldest-first order.
    #[must_use]
    pub fn collect_inscriptions_on_branch(&self, tip: HeaderId) -> Vec<InscriptionInfo> {
        let mut blocks = Vec::new();
        let mut current = tip;

        loop {
            blocks.push(current);
            if current == self.current_lib {
                break;
            }
            match self.parent_map.get(&current) {
                Some(&parent) => current = parent,
                None => break,
            }
        }

        blocks.reverse();
        blocks
            .into_iter()
            .flat_map(|block_id| {
                self.block_inscriptions
                    .get(&block_id)
                    .cloned()
                    .unwrap_or_default()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use lb_core::mantle::{MantleTx, Transaction as _};

    use super::*;

    fn header_id(n: u8) -> HeaderId {
        let mut bytes = [0u8; 32];
        bytes[0] = n;
        HeaderId::from(bytes)
    }

    fn make_dummy_tx(data: u8) -> SignedMantleTx {
        let mantle_tx = MantleTx {
            ops: vec![],
            storage_gas_price: 0.into(),
            execution_gas_price: u64::from(data).into(),
        };
        SignedMantleTx {
            ops_proofs: vec![],
            mantle_tx,
        }
    }

    #[test]
    fn submit_and_query_pending() {
        let genesis = header_id(0);
        let mut state = TxState::new(genesis, MsgId::root());
        let tx = make_dummy_tx(1);

        state.submit_other(tx);
        assert_eq!(state.unfinalized_count(), 1);
    }

    #[test]
    fn block_includes_tx() {
        let genesis = header_id(0);
        let b1 = header_id(1);
        let mut state = TxState::new(genesis, MsgId::root());

        let tx = make_dummy_tx(1);
        let hash = tx.mantle_tx.hash();
        state.submit_other(tx);

        // Process block containing our tx, lib stays at genesis
        state.process_block(b1, genesis, genesis, vec![hash], vec![]);

        // Tx is still pending (not finalized yet, lib hasn't advanced)
        assert_eq!(state.unfinalized_count(), 1);

        // But pending_txs at b1 excludes it (it's in the safe set)
        assert!(state.pending_txs(b1).is_empty());
    }

    #[test]
    fn lib_advance_finalizes() {
        let genesis = header_id(0);
        let b1 = header_id(1);
        let b2 = header_id(2);
        let mut state = TxState::new(genesis, MsgId::root());

        let tx = make_dummy_tx(1);
        let hash = tx.mantle_tx.hash();
        state.submit_other(tx);

        // b1 with our tx
        state.process_block(b1, genesis, genesis, vec![hash], vec![]);
        assert_eq!(state.unfinalized_count(), 1);

        // b2, lib advances to b1 — process_block does not remove from
        // pending (that's done by backfill ground truth)
        state.process_block(b2, b1, b1, vec![], vec![]);
        assert_eq!(
            state.unfinalized_count(),
            1,
            "tx still in pending until backfill confirms"
        );

        // Simulate backfill confirming the tx
        assert!(state.remove_pending(&hash).is_some());
        assert_eq!(state.unfinalized_count(), 0);
    }

    #[test]
    fn pending_txs_excludes_safe() {
        let genesis = header_id(0);
        let b1 = header_id(1);
        let mut state = TxState::new(genesis, MsgId::root());

        let tx1 = make_dummy_tx(1);
        let tx2 = make_dummy_tx(2);
        let hash1 = tx1.mantle_tx.hash();
        let hash2 = tx2.mantle_tx.hash();

        state.submit_other(tx1);
        state.submit_other(tx2);

        // b1 contains only tx1
        state.process_block(b1, genesis, genesis, vec![hash1], vec![]);

        // pending_txs at b1 should only return tx2
        let pending: Vec<_> = state.pending_txs(b1).into_iter().map(|(h, _)| h).collect();
        assert_eq!(pending.len(), 1);
        assert!(pending.contains(&hash2));
    }

    #[test]
    fn reorg_changes_pending_status() {
        // G -> b1 (has tx)
        //   -> b2 (no tx)
        let genesis = header_id(0);
        let b1 = header_id(1);
        let b2 = header_id(2);
        let mut state = TxState::new(genesis, MsgId::root());

        let tx = make_dummy_tx(1);
        let hash = tx.mantle_tx.hash();
        state.submit_other(tx);

        // b1 has our tx
        state.process_block(b1, genesis, genesis, vec![hash], vec![]);

        // At b1 tip, tx is in safe set (not in pending_txs)
        assert!(state.pending_txs(b1).is_empty());

        // b2 forks from genesis, no tx
        state.process_block(b2, genesis, genesis, vec![], vec![]);

        // At b2 tip, tx is back in pending_txs (different branch)
        assert!(state.pending_txs(b2).iter().any(|(h, _)| *h == hash));
    }

    #[test]
    fn lib_advance_prunes_ancestors_and_orphans() {
        // Chain: genesis <- a1 <- a2 <- a3 (lib) <- a4 <- a5 <- a6
        //                    |
        //                   b1 <- b2 (fork from a1)
        let genesis = header_id(0);
        let a1 = header_id(1);
        let a2 = header_id(2);
        let a3 = header_id(3);
        let a4 = header_id(4);
        let a5 = header_id(5);
        let a6 = header_id(6);
        let b1 = header_id(10);
        let b2 = header_id(11);

        let mut state = TxState::new(genesis, MsgId::root());

        // Build main chain up to a1
        state.process_block(a1, genesis, genesis, vec![], vec![]);

        // Build fork from a1 (before lib advances past a1)
        state.process_block(b1, a1, genesis, vec![], vec![]);
        state.process_block(b2, b1, genesis, vec![], vec![]);

        // Verify fork blocks exist before lib advances
        assert!(state.block_states.contains_key(&b1));
        assert!(state.block_states.contains_key(&b2));

        // Continue main chain, lib advances to a3
        state.process_block(a2, a1, genesis, vec![], vec![]);
        state.process_block(a3, a2, a3, vec![], vec![]); // lib advances to a3

        // After lib advances to a3:
        // - genesis, a1, a2 should be pruned (ancestors up to and including old lib)
        // - b1, b2 should be GC'd (orphans - their ancestor a1 was pruned)
        // - a3 (new lib) should exist

        assert!(
            !state.block_states.contains_key(&genesis),
            "genesis (old lib) should be pruned"
        );
        assert!(!state.block_states.contains_key(&a1), "a1 should be pruned");
        assert!(!state.block_states.contains_key(&a2), "a2 should be pruned");
        assert!(
            !state.block_states.contains_key(&b1),
            "orphan b1 should be pruned"
        );
        assert!(
            !state.block_states.contains_key(&b2),
            "orphan b2 should be pruned"
        );

        assert!(state.block_states.contains_key(&a3), "lib should exist");

        // Continue and verify pruning continues working
        state.process_block(a4, a3, a3, vec![], vec![]);
        state.process_block(a5, a4, a5, vec![], vec![]); // lib advances to a5
        state.process_block(a6, a5, a5, vec![], vec![]);

        assert!(
            !state.block_states.contains_key(&a3),
            "old lib should be pruned"
        );
        assert!(!state.block_states.contains_key(&a4), "a4 should be pruned");
        assert!(state.block_states.contains_key(&a5), "new lib should exist");
        assert!(state.block_states.contains_key(&a6), "tip should exist");
    }

    fn msg_id(n: u8) -> MsgId {
        let mut bytes = [0u8; 32];
        bytes[0] = n;
        MsgId::from(bytes)
    }

    /// Submit a fake pending inscription with lineage metadata.
    fn submit_fake_inscription(
        state: &mut TxState,
        data: u8,
        parent_msg: MsgId,
        this_msg: MsgId,
    ) -> TxHash {
        let tx = make_dummy_tx(data);
        let hash = tx.mantle_tx.hash();
        state.submit_inscription(tx, parent_msg, this_msg, vec![data]);
        hash
    }

    #[test]
    fn extension_with_competing_inscription_does_not_orphan_local_pending() {
        // Scenario: local pending b1→b2→b3 from root.
        // Competing c1 lands on chain consuming root as parent.
        // This is an extension — no blocks removed from canonical.
        // Under the block-delta semantics, `orphaned` stays empty; the
        // local pending b1→b2→b3 were never on canonical so they are not
        // reported. They remain in `self.pending` (invalid on current tip,
        // eligible for cleanup when their branch falls below LIB).
        let genesis = header_id(0);
        let block1 = header_id(1);
        let block2 = header_id(2);
        let mut state = TxState::new(genesis, MsgId::root());

        let b1_msg = msg_id(10);
        let b2_msg = msg_id(11);
        let b3_msg = msg_id(12);
        submit_fake_inscription(&mut state, 1, MsgId::root(), b1_msg);
        submit_fake_inscription(&mut state, 2, b1_msg, b2_msg);
        submit_fake_inscription(&mut state, 3, b2_msg, b3_msg);
        assert_eq!(state.pending.len(), 3);

        state.process_block(block1, genesis, genesis, vec![], vec![]);

        let c1_msg = msg_id(20);
        let c1_inscription = InscriptionInfo {
            tx_hash: make_dummy_tx(99).mantle_tx.hash(),
            parent_msg: MsgId::root(),
            this_msg: c1_msg,
            payload: vec![99],
        };
        state.process_block(block2, block1, genesis, vec![], vec![c1_inscription]);

        let update = state
            .detect_channel_update(block1, block2)
            .expect("should detect channel update");

        assert!(update.orphaned.is_empty(), "extension never orphans");
        assert_eq!(update.adopted.len(), 1);
        assert_eq!(update.adopted[0].this_msg, c1_msg);
        // Local pending is still tracked.
        assert_eq!(state.pending.len(), 3);
    }

    #[test]
    fn extension_with_competing_inscription_does_not_orphan_multiple_pending_roots() {
        // Two independent pending inscriptions both target root as parent.
        // Competing c1 lands consuming root. Neither is reported as
        // orphaned under the block-delta semantics; both remain in pending.
        let genesis = header_id(0);
        let block1 = header_id(1);
        let block2 = header_id(2);
        let mut state = TxState::new(genesis, MsgId::root());

        let b1_msg = msg_id(10);
        let d1_msg = msg_id(30);
        submit_fake_inscription(&mut state, 1, MsgId::root(), b1_msg);
        submit_fake_inscription(&mut state, 4, MsgId::root(), d1_msg);

        state.process_block(block1, genesis, genesis, vec![], vec![]);

        let c1_msg = msg_id(20);
        let c1_inscription = InscriptionInfo {
            tx_hash: make_dummy_tx(99).mantle_tx.hash(),
            parent_msg: MsgId::root(),
            this_msg: c1_msg,
            payload: vec![99],
        };
        state.process_block(block2, block1, genesis, vec![], vec![c1_inscription]);

        let update = state.detect_channel_update(block1, block2).unwrap();
        assert!(update.orphaned.is_empty());
        assert_eq!(update.adopted.len(), 1);
        assert_eq!(update.adopted[0].this_msg, c1_msg);
        assert_eq!(state.pending.len(), 2);
    }

    #[test]
    fn fragmented_pending_publish_falls_back_to_canonical() {
        // Two independent pending inscriptions both chain from root.
        // This is ambiguous (2 children of root), so publish_parent
        // should fall back to canonical tip (root), not pick one
        // arbitrarily.
        let genesis = header_id(0);
        let block1 = header_id(1);
        let mut state = TxState::new(genesis, MsgId::root());

        let b1_msg = msg_id(10);
        let d1_msg = msg_id(30);
        submit_fake_inscription(&mut state, 1, MsgId::root(), b1_msg);
        submit_fake_inscription(&mut state, 4, MsgId::root(), d1_msg);

        state.process_block(block1, genesis, genesis, vec![], vec![]);

        // Ambiguous: two children of root → falls back to canonical tip
        assert_eq!(state.publish_parent(block1), MsgId::root());
    }

    #[test]
    fn linear_pending_suffix_extends_from_tail() {
        // Linear pending chain: root → b1 → b2.
        // publish_parent should return b2 (the tail).
        let genesis = header_id(0);
        let block1 = header_id(1);
        let mut state = TxState::new(genesis, MsgId::root());

        let b1_msg = msg_id(10);
        let b2_msg = msg_id(11);
        submit_fake_inscription(&mut state, 1, MsgId::root(), b1_msg);
        submit_fake_inscription(&mut state, 2, b1_msg, b2_msg);

        state.process_block(block1, genesis, genesis, vec![], vec![]);

        assert_eq!(state.publish_parent(block1), b2_msg);
    }

    #[test]
    fn stale_pending_tail_not_reused_for_publish() {
        // Local pending b1 from root. c1 lands consuming root.
        // publish_parent should return c1 (canonical tip), not b1.
        let genesis = header_id(0);
        let block1 = header_id(1);
        let block2 = header_id(2);
        let mut state = TxState::new(genesis, MsgId::root());

        let b1_msg = msg_id(10);
        submit_fake_inscription(&mut state, 1, MsgId::root(), b1_msg);

        state.process_block(block1, genesis, genesis, vec![], vec![]);

        // c1 lands, consuming root
        let c1_msg = msg_id(20);
        let c1_inscription = InscriptionInfo {
            tx_hash: make_dummy_tx(99).mantle_tx.hash(),
            parent_msg: MsgId::root(),
            this_msg: c1_msg,
            payload: vec![99],
        };
        state.process_block(block2, block1, genesis, vec![], vec![c1_inscription]);

        // b1 is stale — publish_parent should return canonical tip (c1)
        assert_eq!(state.publish_parent(block2), c1_msg);
    }

    #[test]
    fn multi_block_lib_advance_finalizes_intermediate() {
        // When LIB advances multiple blocks at once, all intermediate txs must finalize
        // genesis <- b1 (tx1) <- b2 (tx2) <- b3
        //                                     ^
        //                                    LIB jumps here
        let genesis = header_id(0);
        let b1 = header_id(1);
        let b2 = header_id(2);
        let b3 = header_id(3);
        let mut state = TxState::new(genesis, MsgId::root());

        let tx1 = make_dummy_tx(1);
        let tx2 = make_dummy_tx(2);
        let hash1 = tx1.mantle_tx.hash();
        let hash2 = tx2.mantle_tx.hash();

        state.submit_other(tx1);
        state.submit_other(tx2);

        // b1 has tx1
        state.process_block(b1, genesis, genesis, vec![hash1], vec![]);
        // b2 has tx2
        state.process_block(b2, b1, genesis, vec![hash2], vec![]);
        // b3, lib jumps from genesis to b2 (skipping b1)
        state.process_block(b3, b2, b2, vec![], vec![]);
        assert_eq!(
            state.unfinalized_count(),
            2,
            "txs still pending until backfill"
        );

        // Simulate backfill confirming both txs
        assert!(state.remove_pending(&hash1).is_some());
        assert!(state.remove_pending(&hash2).is_some());
        assert_eq!(state.unfinalized_count(), 0);
    }
}
