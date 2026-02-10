use std::collections::{BTreeMap, HashMap, HashSet};

use lb_core::{
    header::HeaderId,
    mantle::{SignedMantleTx, tx::TxHash},
};
use rpds::HashTrieSetSync;

/// Transaction status in the lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxStatus {
    /// Not yet on canonical chain, needs resubmitting.
    Pending,
    /// On canonical chain between LIB and tip.
    Safe,
    /// At or below LIB, permanent.
    Finalized,
    /// Unknown transaction.
    Unknown,
}

/// Transaction state tracker.
pub struct TxState {
    /// All transactions being tracked, kept until finalized.
    pending: HashMap<TxHash, SignedMantleTx>,
    /// Per-block cumulative safe sets.
    block_states: BTreeMap<HeaderId, HashTrieSetSync<TxHash>>,
    /// Block parent relationships for pruning.
    parent_map: HashMap<HeaderId, HeaderId>,
    /// Finalized transactions.
    finalized: HashSet<TxHash>,
    /// Current LIB for pruning.
    current_lib: HeaderId,
}

impl TxState {
    #[must_use]
    pub fn new(lib: HeaderId) -> Self {
        let mut block_states = BTreeMap::new();
        block_states.insert(lib, HashTrieSetSync::new_sync());
        Self {
            pending: HashMap::new(),
            block_states,
            parent_map: HashMap::new(),
            finalized: HashSet::new(),
            current_lib: lib,
        }
    }

    /// Submit a transaction for tracking.
    pub fn submit(&mut self, tx_hash: TxHash, signed_tx: SignedMantleTx) {
        self.pending.insert(tx_hash, signed_tx);
    }

    /// Process a new block.
    pub fn process_block(
        &mut self,
        block_id: HeaderId,
        parent_id: HeaderId,
        lib: HeaderId,
        our_txs: impl IntoIterator<Item = TxHash>,
    ) {
        // Store parent relationship for pruning
        self.parent_map.insert(block_id, parent_id);

        // Build cumulative safe set from parent
        // TODO: implement backfilling for missing blocks
        let mut safe_set = self
            .block_states
            .get(&parent_id)
            .cloned()
            .expect("parent state should exist");

        for tx in our_txs {
            if self.pending.contains_key(&tx) {
                safe_set = safe_set.insert(tx);
            }
        }
        self.block_states.insert(block_id, safe_set);

        // When lib advances: finalize txs and prune
        if lib != self.current_lib {
            // Finalize txs in all blocks from new lib back to old lib (inclusive)
            let mut block_opt = Some(lib);
            while let Some(block) = block_opt {
                let block_safe = self
                    .block_states
                    .get(&block)
                    .expect("block state should exist for blocks between old LIB and new LIB");

                for tx_hash in block_safe.iter() {
                    if self.pending.remove(tx_hash).is_some() {
                        self.finalized.insert(*tx_hash);
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
                self.parent_map.remove(&orphan);
            }
        }
    }

    #[must_use]
    pub fn status(&self, tx_hash: &TxHash, tip: HeaderId) -> TxStatus {
        if self.finalized.contains(tx_hash) {
            return TxStatus::Finalized;
        }

        if let Some(safe_set) = self.block_states.get(&tip)
            && safe_set.contains(tx_hash)
        {
            return TxStatus::Safe;
        }

        if self.pending.contains_key(tx_hash) {
            return TxStatus::Pending;
        }

        TxStatus::Unknown
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
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Number of finalized transactions.
    #[must_use]
    pub fn finalized_count(&self) -> usize {
        self.finalized.len()
    }
}

#[cfg(test)]
mod tests {
    use lb_core::mantle::{MantleTx, Transaction as _, ledger::Tx as LedgerTx};

    use super::*;

    fn header_id(n: u8) -> HeaderId {
        let mut bytes = [0u8; 32];
        bytes[0] = n;
        HeaderId::from(bytes)
    }

    fn make_dummy_tx(data: u8) -> SignedMantleTx {
        let ledger_tx = LedgerTx::new(vec![], vec![]);
        let mantle_tx = MantleTx {
            ops: vec![],
            ledger_tx,
            storage_gas_price: 0,
            execution_gas_price: data.into(),
        };
        SignedMantleTx {
            ops_proofs: vec![],
            ledger_tx_proof: lb_key_management_system_service::keys::ZkKey::multi_sign(
                &[],
                mantle_tx.hash().as_ref(),
            )
            .expect("empty multi-sign"),
            mantle_tx,
        }
    }

    #[test]
    fn submit_and_query_pending() {
        let genesis = header_id(0);
        let mut state = TxState::new(genesis);
        let tx = make_dummy_tx(1);
        let hash = tx.mantle_tx.hash();

        state.submit(hash, tx);
        assert_eq!(state.pending_count(), 1);
        assert_eq!(state.status(&hash, genesis), TxStatus::Pending);
    }

    #[test]
    fn block_promotes_to_safe() {
        let genesis = header_id(0);
        let b1 = header_id(1);
        let mut state = TxState::new(genesis);

        let tx = make_dummy_tx(1);
        let hash = tx.mantle_tx.hash();
        state.submit(hash, tx);

        // Process block containing our tx, lib stays at genesis
        state.process_block(b1, genesis, genesis, vec![hash]);

        assert_eq!(state.status(&hash, b1), TxStatus::Safe);
        assert_eq!(state.status(&hash, genesis), TxStatus::Pending);
    }

    #[test]
    fn lib_advance_finalizes() {
        let genesis = header_id(0);
        let b1 = header_id(1);
        let b2 = header_id(2);
        let mut state = TxState::new(genesis);

        let tx = make_dummy_tx(1);
        let hash = tx.mantle_tx.hash();
        state.submit(hash, tx);

        // b1 with our tx
        state.process_block(b1, genesis, genesis, vec![hash]);
        assert_eq!(state.status(&hash, b1), TxStatus::Safe);

        // b2, lib advances to b1
        state.process_block(b2, b1, b1, vec![]);
        assert_eq!(state.status(&hash, b2), TxStatus::Finalized);
        assert_eq!(state.finalized_count(), 1);
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

        state.submit(hash1, tx1);
        state.submit(hash2, tx2);

        // b1 contains only tx1
        state.process_block(b1, genesis, genesis, vec![hash1]);

        // pending_txs at b1 should only return tx2
        let pending: Vec<_> = state.pending_txs(b1).map(|(h, _)| *h).collect();
        assert_eq!(pending.len(), 1);
        assert!(pending.contains(&hash2));
    }

    #[test]
    fn reorg_changes_safe_status() {
        // G -> b1 (has tx)
        //   -> b2 (no tx)
        let genesis = header_id(0);
        let b1 = header_id(1);
        let b2 = header_id(2);
        let mut state = TxState::new(genesis);

        let tx = make_dummy_tx(1);
        let hash = tx.mantle_tx.hash();
        state.submit(hash, tx);

        // b1 has our tx
        state.process_block(b1, genesis, genesis, vec![hash]);
        assert_eq!(state.status(&hash, b1), TxStatus::Safe);

        // b2 forks from genesis, no tx
        state.process_block(b2, genesis, genesis, vec![]);

        // At b2 tip, tx is not safe (different branch)
        assert_eq!(state.status(&hash, b2), TxStatus::Pending);
        // At b1 tip, tx is still safe
        assert_eq!(state.status(&hash, b1), TxStatus::Safe);
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

        state.submit(hash1, tx1);
        state.submit(hash2, tx2);

        // b1 has tx1
        state.process_block(b1, genesis, genesis, vec![hash1]);
        // b2 has tx2
        state.process_block(b2, b1, genesis, vec![hash2]);
        // b3, lib jumps from genesis to b2 (skipping b1)
        state.process_block(b3, b2, b2, vec![]);

        // Both tx1 (in b1) and tx2 (in b2) should be finalized
        assert_eq!(state.status(&hash1, b3), TxStatus::Finalized);
        assert_eq!(state.status(&hash2, b3), TxStatus::Finalized);
        assert_eq!(state.finalized_count(), 2);
    }
}
