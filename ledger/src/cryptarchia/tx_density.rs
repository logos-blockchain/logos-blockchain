//! Per-epoch block and transaction counts feeding the Blend `PoW` difficulty.
//!
//! The Blend difficulty is retargeted from the *average number of
//! transactions per block* over a whole epoch. An epoch's totals are only
//! usable once the epoch has ended, and only once its last blocks are deep
//! enough to be beyond re-org: they are therefore closed at the epoch
//! boundary and consumed later, at the nonce snapshot slot of the epoch after
//! that.

/// Totals accumulated over a single epoch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct EpochTotals {
    blocks: u64,
    txs: u64,
}

impl EpochTotals {
    const fn record_block(&mut self, txs_in_block: u64) {
        self.blocks = self.blocks.saturating_add(1);
        self.txs = self.txs.saturating_add(txs_in_block);
    }
}

/// The rolling block and transaction counts of the epoch being extended and of
/// the last epoch that closed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TxDensity {
    /// Totals of the epoch the ledger is currently extending, still growing
    /// with every applied block.
    current_epoch: EpochTotals,
    /// Totals of the most recently closed epoch, final and no longer moving.
    last_closed_epoch: EpochTotals,
}

impl TxDensity {
    /// Count a block, and the transactions it carried, into the current
    /// epoch's totals.
    pub(super) const fn record_block(&mut self, txs_in_block: u64) {
        self.current_epoch.record_block(txs_in_block);
    }

    /// Close the current epoch's totals and start counting a new epoch.
    ///
    /// Called once per epoch crossed, so that epochs skipped entirely (no
    /// block was produced in them) close as empty and are read as zero
    /// demand.
    pub(super) const fn close_epoch(&mut self) {
        self.last_closed_epoch = self.current_epoch;
        self.current_epoch = EpochTotals { blocks: 0, txs: 0 };
    }

    /// The totals — `(transactions, blocks)` — of the last closed epoch, the
    /// observation the Blend difficulty retarget is computed from.
    pub(super) const fn last_closed_epoch_totals(&self) -> (u64, u64) {
        (self.last_closed_epoch.txs, self.last_closed_epoch.blocks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals_accumulate_until_the_epoch_closes() {
        let mut density = TxDensity::default();
        assert_eq!(density.last_closed_epoch_totals(), (0, 0));

        density.record_block(3);
        density.record_block(0);
        density.record_block(7);
        // Still open: nothing is observable yet.
        assert_eq!(density.last_closed_epoch_totals(), (0, 0));

        density.close_epoch();
        assert_eq!(density.last_closed_epoch_totals(), (10, 3));
    }

    #[test]
    fn blocks_of_the_new_epoch_do_not_move_the_closed_totals() {
        let mut density = TxDensity::default();
        density.record_block(5);
        density.close_epoch();
        density.record_block(100);
        assert_eq!(density.last_closed_epoch_totals(), (5, 1));
    }

    #[test]
    fn a_skipped_epoch_closes_as_empty() {
        // Two epochs crossed at once: the first close hands over the real
        // totals, the second closes the epoch that had no blocks at all.
        let mut density = TxDensity::default();
        density.record_block(5);
        density.close_epoch();
        density.close_epoch();
        assert_eq!(density.last_closed_epoch_totals(), (0, 0));
    }
}
