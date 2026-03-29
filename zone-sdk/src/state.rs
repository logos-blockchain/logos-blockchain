use std::collections::{BTreeMap, HashMap};

use lb_core::{
    header::HeaderId,
    mantle::{SignedMantleTx, ops::channel::MsgId, tx::TxHash},
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

/// Result of channel update detection.
///
/// - `adopted`: newly canonical inscriptions since the last common message.
/// - `invalidated`: local pending inscriptions whose parent is no longer
///   canonical.
/// - When `invalidated` is empty, this is an extension-only update.
#[derive(Debug)]
pub struct ChannelUpdateInfo {
    /// Our pending inscriptions that are now invalid (parent taken).
    pub invalidated: Vec<InscriptionInfo>,
    /// New inscriptions that appeared on chain (from LCM to new tip).
    pub adopted: Vec<InscriptionInfo>,
    /// The new channel tip MsgId.
    pub new_channel_tip: MsgId,
}

impl ChannelUpdateInfo {
    /// Returns true if this update invalidated pending inscriptions,
    /// meaning a competing inscription or L1 reorg broke our pending chain.
    #[must_use]
    pub fn is_conflict(&self) -> bool {
        !self.invalidated.is_empty()
    }
}

/// Transaction state tracker.
pub struct TxState {
    /// All transactions being tracked, kept until finalized.
    pending: HashMap<TxHash, SignedMantleTx>,
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
            block_states,
            parent_map: HashMap::new(),
            current_lib: lib,
            block_inscriptions: HashMap::new(),
            finalized_msg,
        }
    }

    /// Last finalized channel tip MsgId.
    #[must_use]
    pub const fn finalized_msg(&self) -> MsgId {
        self.finalized_msg
    }

    /// Submit a transaction for tracking.
    pub fn submit(&mut self, tx_hash: TxHash, signed_tx: SignedMantleTx) {
        self.pending.insert(tx_hash, signed_tx);
    }

    /// Process a new block. Returns newly finalized tx hashes.
    pub fn process_block(
        &mut self,
        block_id: HeaderId,
        parent_id: HeaderId,
        lib: HeaderId,
        our_txs: impl IntoIterator<Item = TxHash>,
        inscriptions: Vec<InscriptionInfo>,
    ) -> Vec<TxHash> {
        // Store parent relationship for pruning
        self.parent_map.insert(block_id, parent_id);

        // Build cumulative safe set from parent.
        // If parent state is missing (e.g., first event after subscribe is a snapshot
        // where we receive a block whose parent we never saw), start with an empty set.
        // This is conservative: txs might show as "pending" when they should be "safe",
        // but they'll be correctly detected when seen in subsequent blocks.
        let parent_safe_exists = self.block_states.contains_key(&parent_id);
        let mut safe_set = self
            .block_states
            .get(&parent_id)
            .cloned()
            .unwrap_or_default();

        let mut added_to_safe = 0;
        for tx in our_txs {
            if self.pending.contains_key(&tx) {
                safe_set = safe_set.insert(tx);
                added_to_safe += 1;
            }
        }
        self.block_states.insert(block_id, safe_set.clone());
        if added_to_safe > 0 || !parent_safe_exists {
            eprintln!("[SEQ] Block {block_id:?}: added {added_to_safe} txs to safe set (total safe={}, parent_exists={parent_safe_exists})", safe_set.size());
        }

        // Store channel inscriptions for this block
        if !inscriptions.is_empty() {
            self.block_inscriptions.insert(block_id, inscriptions);
        }

        let mut newly_finalized = Vec::new();

        // When lib advances: finalize txs and prune
        if lib != self.current_lib {
            eprintln!("[SEQ] LIB advanced: {:?} -> {:?}, pending={}", self.current_lib, lib, self.pending.len());
            // Walk from new LIB back to old LIB via parent_map.
            // Finalize pending txs found in safe sets along this path.
            let mut walk_count = 0;
            let mut block_opt = Some(lib);
            while let Some(block) = block_opt {
                walk_count += 1;
                if let Some(block_safe) = self.block_states.get(&block) {
                    for tx_hash in block_safe.iter() {
                        if self.pending.remove(tx_hash).is_some() {
                            newly_finalized.push(*tx_hash);
                        }
                    }
                }
                if block == self.current_lib {
                    break;
                }
                block_opt = self.parent_map.get(&block).copied();
            }
            eprintln!("[SEQ] Finalization walk: walked {walk_count} blocks, finalized {}", newly_finalized.len());

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

            // Rebuild the safe set at LIB to contain only still-pending tx
            // hashes. This breaks the rpds sharing chain with pruned ancestors,
            // preventing unbounded accumulation of finalized hashes in the
            // persistent structure.
            if let Some(lib_safe_set) = self.block_states.get(&lib) {
                let mut fresh = HashTrieSetSync::new_sync();
                for hash in lib_safe_set.iter() {
                    if self.pending.contains_key(hash) {
                        fresh = fresh.insert(*hash);
                    }
                }
                self.block_states.insert(lib, fresh);
            }

            self.prune_orphans(lib);
            self.current_lib = lib;
        }

        newly_finalized
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

    /// Pending txs for resubmission (not safe at tip).
    pub fn pending_txs(&self, tip: HeaderId) -> impl Iterator<Item = (&TxHash, &SignedMantleTx)> {
        let safe = self
            .block_states
            .get(&tip)
            .cloned()
            .unwrap_or_else(HashTrieSetSync::new_sync);
        self.pending
            .iter()
            .filter(move |(hash, _)| !safe.contains(hash))
    }

    /// Number of pending transactions (all types).
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Whether there are pending channel inscriptions (not counting set_keys
    /// or other non-inscription ops).
    #[must_use]
    pub fn has_pending_inscriptions(&self) -> bool {
        self.pending.values().any(|tx| {
            tx.mantle_tx
                .ops
                .iter()
                .any(|op| matches!(op, lb_core::mantle::ops::Op::ChannelInscribe(_)))
        })
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
    pub fn all_pending_txs(&self) -> impl Iterator<Item = (&TxHash, &SignedMantleTx)> {
        self.pending.iter()
    }

    /// Remove a pending transaction and return it.
    pub fn remove_pending(&mut self, tx_hash: &TxHash) -> Option<SignedMantleTx> {
        self.pending.remove(tx_hash)
    }

    /// Derive the channel tip MsgId at a given L1 block by walking backwards
    /// through the block tree and finding the most recent inscription.
    /// Returns `finalized_msg` if no inscriptions are found in the
    /// unfinalized window.
    #[must_use]
    pub fn channel_tip_at(&self, block_id: HeaderId) -> MsgId {
        let mut current = block_id;
        loop {
            if let Some(inscs) = self.block_inscriptions.get(&current) {
                if let Some(last) = inscs.last() {
                    return last.this_msg;
                }
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
    /// - Extension: channel tip in new_tip extends from old_tip
    ///   → report adopted inscriptions, no invalidation
    /// - Reorg: channel tips diverged → find LCM, orphan entire pending
    ///   suffix from LCM, report adopted from LCM to new tip
    ///
    /// Returns `None` if no channel state change.
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

        // Build set of msg IDs on the new canonical channel chain.
        let new_chain: std::collections::HashSet<MsgId> = new_branch
            .iter()
            .map(|i| i.this_msg)
            .chain(std::iter::once(self.finalized_msg))
            .collect();

        // Extension check: old channel tip is an ancestor of new channel tip.
        let extends = new_chain.contains(&old_channel_tip);

        // Find LCM — latest msg that exists on both channel chains.
        let lcm = old_branch
            .iter()
            .rev()
            .find(|i| new_chain.contains(&i.this_msg))
            .map(|i| i.this_msg)
            .unwrap_or(self.finalized_msg);

        // Adopted: inscriptions on the new branch after the LCM.
        let adopted: Vec<InscriptionInfo> =
            if let Some(start_idx) = new_branch.iter().position(|i| i.parent_msg == lcm) {
                new_branch[start_idx..].to_vec()
            } else {
                Vec::new()
            };

        if extends && adopted.is_empty() {
            return None;
        }

        // Invalidated: on extension nothing is orphaned. On reorg, the
        // entire pending suffix from LCM is orphaned.
        let invalidated = if extends {
            Vec::new()
        } else {
            self.collect_pending_suffix(lcm)
        };

        Some(ChannelUpdateInfo {
            invalidated,
            adopted,
            new_channel_tip,
        })
    }

    /// Collect the pending inscription suffix that chains from `from_msg`.
    /// Walks the pending txs following parent→child links transitively.
    pub(crate) fn collect_pending_suffix(&self, from_msg: MsgId) -> Vec<InscriptionInfo> {
        let mut suffix = Vec::new();
        let mut frontier = vec![from_msg];

        // BFS: find all pending inscriptions reachable from from_msg
        while let Some(parent) = frontier.pop() {
            for (tx_hash, signed_tx) in &self.pending {
                for op in &signed_tx.mantle_tx.ops {
                    if let lb_core::mantle::ops::Op::ChannelInscribe(inscribe) = op {
                        if inscribe.parent == parent {
                            suffix.push(InscriptionInfo {
                                tx_hash: *tx_hash,
                                parent_msg: inscribe.parent,
                                this_msg: inscribe.id(),
                                payload: inscribe.inscription.clone(),
                            });
                            frontier.push(inscribe.id());
                        }
                    }
                }
            }
        }

        suffix
    }

    /// Collect all inscriptions on a branch from the given block back to LIB,
    /// in oldest-first order.
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
            storage_gas_price: 0,
            execution_gas_price: data.into(),
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
        let hash = tx.mantle_tx.hash();

        state.submit(hash, tx);
        assert_eq!(state.pending_count(), 1);
    }

    #[test]
    fn block_includes_tx() {
        let genesis = header_id(0);
        let b1 = header_id(1);
        let mut state = TxState::new(genesis, MsgId::root());

        let tx = make_dummy_tx(1);
        let hash = tx.mantle_tx.hash();
        state.submit(hash, tx);

        // Process block containing our tx, lib stays at genesis
        state.process_block(b1, genesis, genesis, vec![hash], vec![]);

        // Tx is still pending (not finalized yet, lib hasn't advanced)
        assert_eq!(state.pending_count(), 1);

        // But pending_txs at b1 excludes it (it's in the safe set)
        assert!(state.pending_txs(b1).next().is_none());
    }

    #[test]
    fn lib_advance_finalizes() {
        let genesis = header_id(0);
        let b1 = header_id(1);
        let b2 = header_id(2);
        let mut state = TxState::new(genesis, MsgId::root());

        let tx = make_dummy_tx(1);
        let hash = tx.mantle_tx.hash();
        state.submit(hash, tx);

        // b1 with our tx
        state.process_block(b1, genesis, genesis, vec![hash], vec![]);
        assert_eq!(state.pending_count(), 1);

        // b2, lib advances to b1
        let finalized = state.process_block(b2, b1, b1, vec![], vec![]);
        assert_eq!(finalized, vec![hash]);
        assert_eq!(state.pending_count(), 0);
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

        state.submit(hash1, tx1);
        state.submit(hash2, tx2);

        // b1 contains only tx1
        state.process_block(b1, genesis, genesis, vec![hash1], vec![]);

        // pending_txs at b1 should only return tx2
        let pending: Vec<_> = state.pending_txs(b1).map(|(h, _)| *h).collect();
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
        state.submit(hash, tx);

        // b1 has our tx
        state.process_block(b1, genesis, genesis, vec![hash], vec![]);

        // At b1 tip, tx is in safe set (not in pending_txs)
        assert!(state.pending_txs(b1).next().is_none());

        // b2 forks from genesis, no tx
        state.process_block(b2, genesis, genesis, vec![], vec![]);

        // At b2 tip, tx is back in pending_txs (different branch)
        assert!(state.pending_txs(b2).any(|(h, _)| *h == hash));
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

        state.submit(hash1, tx1);
        state.submit(hash2, tx2);

        // b1 has tx1
        state.process_block(b1, genesis, genesis, vec![hash1], vec![]);
        // b2 has tx2
        state.process_block(b2, b1, genesis, vec![hash2], vec![]);
        // b3, lib jumps from genesis to b2 (skipping b1)
        let finalized = state.process_block(b3, b2, b2, vec![], vec![]);

        // Both tx1 (in b1) and tx2 (in b2) should be finalized
        assert!(finalized.contains(&hash1));
        assert!(finalized.contains(&hash2));
        assert_eq!(finalized.len(), 2);
        assert_eq!(state.pending_count(), 0);
    }
}
