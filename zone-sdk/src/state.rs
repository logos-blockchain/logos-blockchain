use std::collections::HashMap;

use lb_core::mantle::{SignedMantleTx, Transaction as _, tx::TxHash};

/// Pure in-memory state tracker for zone transactions.
///
/// Tracks pending transactions (submitted but not yet finalized).
/// When a LIB block is processed, any pending txs found in that block
/// are removed from pending and returned as newly finalized.
pub struct State {
    pending: HashMap<TxHash, SignedMantleTx>,
}

impl State {
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
        }
    }

    /// Restore state from a set of pending transactions (e.g. from a
    /// checkpoint).
    #[must_use]
    pub const fn from_pending(pending: HashMap<TxHash, SignedMantleTx>) -> Self {
        Self { pending }
    }

    /// Add a transaction to the pending set.
    pub fn submit_pending(&mut self, tx_hash: TxHash, signed_tx: SignedMantleTx) {
        self.pending.insert(tx_hash, signed_tx);
    }

    /// Returns an iterator over all pending transactions (for resubmission).
    pub fn pending_txs(&self) -> impl Iterator<Item = (&TxHash, &SignedMantleTx)> {
        self.pending.iter()
    }

    /// Returns the number of pending transactions.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Process a finalized (LIB) block.
    ///
    /// Scans the block's transactions, intersects with the pending set,
    /// removes matches, and returns the tx hashes that were finalized.
    pub fn process_lib_block(&mut self, txs: &[SignedMantleTx]) -> Vec<TxHash> {
        let mut finalized = Vec::new();
        for tx in txs {
            let tx_hash = tx.mantle_tx.hash();
            if self.pending.remove(&tx_hash).is_some() {
                finalized.push(tx_hash);
            }
        }
        finalized
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use lb_core::mantle::{MantleTx, Transaction as _, ledger::Tx as LedgerTx};

    use super::*;

    fn make_dummy_tx(data: &[u8]) -> SignedMantleTx {
        let ledger_tx = LedgerTx::new(vec![], vec![]);
        let mantle_tx = MantleTx {
            ops: vec![],
            ledger_tx,
            storage_gas_price: 0,
            execution_gas_price: data.first().copied().unwrap_or(0).into(),
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
        let mut state = State::new();
        let tx = make_dummy_tx(&[1]);
        let tx_hash = tx.mantle_tx.hash();

        state.submit_pending(tx_hash, tx);
        assert_eq!(state.pending_count(), 1);
        assert!(state.pending_txs().any(|(h, _)| *h == tx_hash));
    }

    #[test]
    fn default_is_empty() {
        let state = State::default();
        assert_eq!(state.pending_count(), 0);
    }

    #[test]
    fn process_lib_block_finalizes_pending() {
        let mut state = State::new();
        let tx_a = make_dummy_tx(&[1]);
        let tx_b = make_dummy_tx(&[2]);
        let tx_c = make_dummy_tx(&[3]); // not submitted as pending
        let hash_a = tx_a.mantle_tx.hash();
        let hash_b = tx_b.mantle_tx.hash();

        state.submit_pending(hash_a, tx_a.clone());
        state.submit_pending(hash_b, tx_b);
        assert_eq!(state.pending_count(), 2);

        // Block contains tx_a and tx_c (tx_c was never pending)
        let finalized = state.process_lib_block(&[tx_a, tx_c]);

        assert_eq!(finalized, vec![hash_a]);
        assert_eq!(state.pending_count(), 1);
        assert!(state.pending_txs().any(|(h, _)| *h == hash_b));
    }

    #[test]
    fn process_lib_block_empty_block() {
        let mut state = State::new();
        let tx = make_dummy_tx(&[1]);
        let tx_hash = tx.mantle_tx.hash();
        state.submit_pending(tx_hash, tx);

        let finalized = state.process_lib_block(&[]);
        assert!(finalized.is_empty());
        assert_eq!(state.pending_count(), 1);
    }

    #[test]
    fn from_pending_restores() {
        let tx = make_dummy_tx(&[2]);
        let tx_hash = tx.mantle_tx.hash();
        let mut pending = HashMap::new();
        pending.insert(tx_hash, tx);

        let state = State::from_pending(pending);
        assert_eq!(state.pending_count(), 1);
    }
}
