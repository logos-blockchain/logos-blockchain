use std::collections::{BTreeMap, HashMap};

use indexmap::IndexMap;
use lb_core::{
    header::HeaderId,
    mantle::{SignedMantleTx, Transaction as _, tx::TxHash},
};
use rpds::HashTrieSetSync;

/// Transaction state tracker.
pub struct TxState {
    /// All transactions being tracked, kept until finalized.
    pending: IndexMap<TxHash, SignedMantleTx>,
    /// Per-block cumulative safe sets.
    block_states: BTreeMap<HeaderId, HashTrieSetSync<TxHash>>,
    /// Block parent relationships for pruning.
    parent_map: HashMap<HeaderId, HeaderId>,
    /// Current LIB for pruning.
    current_lib: HeaderId,
}

impl TxState {
    #[must_use]
    pub fn new(lib: HeaderId) -> Self {
        let mut block_states = BTreeMap::new();
        block_states.insert(lib, HashTrieSetSync::new_sync());
        Self {
            pending: IndexMap::new(),
            block_states,
            parent_map: HashMap::new(),
            current_lib: lib,
        }
    }

    /// Submit a transaction for tracking.
    pub fn submit(&mut self, signed_tx: SignedMantleTx) {
        let tx_hash = signed_tx.mantle_tx.hash();
        self.pending.insert(tx_hash, signed_tx);
    }

    /// Process a new block. Returns newly finalized tx hashes.
    pub fn process_block(
        &mut self,
        block_id: HeaderId,
        parent_id: HeaderId,
        lib: HeaderId,
        our_txs: impl IntoIterator<Item = TxHash>,
    ) -> Vec<TxHash> {
        // Store parent relationship for pruning
        self.parent_map.insert(block_id, parent_id);

        // Build cumulative safe set from parent. Parent may be missing
        // during slot-range backfill when blocks reference parents outside
        // the range. Starting with empty is conservative but safe.
        let mut safe_set = self
            .block_states
            .get(&parent_id)
            .cloned()
            .unwrap_or_default();

        for tx in our_txs {
            if self.pending.contains_key(&tx) {
                safe_set = safe_set.insert(tx);
            }
        }
        self.block_states.insert(block_id, safe_set);

        let mut newly_finalized = Vec::new();

        // When lib advances: finalize txs and prune
        if lib != self.current_lib {
            // Finalize txs in all blocks from new lib back to old lib (inclusive).
            // We may not have state for all intermediate blocks if we missed events,
            // so we skip blocks we don't know about.
            let mut block_opt = Some(lib);
            while let Some(block) = block_opt {
                if let Some(block_safe) = self.block_states.get(&block) {
                    for tx_hash in block_safe.iter() {
                        if self.pending.shift_remove(tx_hash).is_some() {
                            newly_finalized.push(*tx_hash);
                        }
                    }
                }

                if block == self.current_lib {
                    break;
                }

                block_opt = self.parent_map.get(&block).copied();
            }

            // Prune ancestors of new lib (but not lib itself)
            let mut prune_cursor = self.parent_map.get(&lib).copied();
            while let Some(b) = prune_cursor {
                self.block_states.remove(&b);
                prune_cursor = self.parent_map.remove(&b);
            }

            // Remove finalized tx hashes from all safe sets. Using remove
            // (rather than rebuild) preserves rpds memory sharing between
            // block states for non-finalized txs.
            for safe_set in self.block_states.values_mut() {
                for tx_hash in &newly_finalized {
                    *safe_set = safe_set.remove(tx_hash);
                }
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

    /// Number of pending transactions.
    #[must_use]
    pub fn unfinalized_count(&self) -> usize {
        self.pending.len()
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
        let mut state = TxState::new(genesis);
        let tx = make_dummy_tx(1);

        state.submit(tx);
        assert_eq!(state.unfinalized_count(), 1);
    }

    #[test]
    fn block_includes_tx() {
        let genesis = header_id(0);
        let b1 = header_id(1);
        let mut state = TxState::new(genesis);

        let tx = make_dummy_tx(1);
        let hash = tx.mantle_tx.hash();
        state.submit(tx);

        // Process block containing our tx, lib stays at genesis
        state.process_block(b1, genesis, genesis, vec![hash]);

        // Tx is still pending (not finalized yet, lib hasn't advanced)
        assert_eq!(state.unfinalized_count(), 1);

        // But pending_txs at b1 excludes it (it's in the safe set)
        assert!(state.pending_txs(b1).next().is_none());
    }

    #[test]
    fn lib_advance_finalizes() {
        let genesis = header_id(0);
        let b1 = header_id(1);
        let b2 = header_id(2);
        let mut state = TxState::new(genesis);

        let tx = make_dummy_tx(1);
        let hash = tx.mantle_tx.hash();
        state.submit(tx);

        // b1 with our tx
        state.process_block(b1, genesis, genesis, vec![hash]);
        assert_eq!(state.unfinalized_count(), 1);

        // b2, lib advances to b1
        let finalized = state.process_block(b2, b1, b1, vec![]);
        assert_eq!(finalized, vec![hash]);
        assert_eq!(state.unfinalized_count(), 0);
    }

    #[test]
    fn pending_txs_excludes_safe() {
        let genesis = header_id(0);
        let b1 = header_id(1);
        let mut state = TxState::new(genesis);

        let tx1 = make_dummy_tx(1);
        let tx2 = make_dummy_tx(2);
        let hash1 = tx1.mantle_tx.hash();
        let hash2 = tx2.mantle_tx.hash();

        state.submit(tx1);
        state.submit(tx2);

        // b1 contains only tx1
        state.process_block(b1, genesis, genesis, vec![hash1]);

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
        let mut state = TxState::new(genesis);

        let tx = make_dummy_tx(1);
        let hash = tx.mantle_tx.hash();
        state.submit(tx);

        // b1 has our tx
        state.process_block(b1, genesis, genesis, vec![hash]);

        // At b1 tip, tx is in safe set (not in pending_txs)
        assert!(state.pending_txs(b1).next().is_none());

        // b2 forks from genesis, no tx
        state.process_block(b2, genesis, genesis, vec![]);

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

        let mut state = TxState::new(genesis);

        // Build main chain up to a1
        state.process_block(a1, genesis, genesis, vec![]);

        // Build fork from a1 (before lib advances past a1)
        state.process_block(b1, a1, genesis, vec![]);
        state.process_block(b2, b1, genesis, vec![]);

        // Verify fork blocks exist before lib advances
        assert!(state.block_states.contains_key(&b1));
        assert!(state.block_states.contains_key(&b2));

        // Continue main chain, lib advances to a3
        state.process_block(a2, a1, genesis, vec![]);
        state.process_block(a3, a2, a3, vec![]); // lib advances to a3

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
        state.process_block(a4, a3, a3, vec![]);
        state.process_block(a5, a4, a5, vec![]); // lib advances to a5
        state.process_block(a6, a5, a5, vec![]);

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
        let mut state = TxState::new(genesis);

        let tx1 = make_dummy_tx(1);
        let tx2 = make_dummy_tx(2);
        let hash1 = tx1.mantle_tx.hash();
        let hash2 = tx2.mantle_tx.hash();

        state.submit(tx1);
        state.submit(tx2);

        // b1 has tx1
        state.process_block(b1, genesis, genesis, vec![hash1]);
        // b2 has tx2
        state.process_block(b2, b1, genesis, vec![hash2]);
        // b3, lib jumps from genesis to b2 (skipping b1)
        let finalized = state.process_block(b3, b2, b2, vec![]);

        // Both tx1 (in b1) and tx2 (in b2) should be finalized
        assert!(finalized.contains(&hash1));
        assert!(finalized.contains(&hash2));
        assert_eq!(finalized.len(), 2);
        assert_eq!(state.unfinalized_count(), 0);
    }
}
