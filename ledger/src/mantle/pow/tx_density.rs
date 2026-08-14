//! Per-epoch block and transaction counts feeding the Blend `PoW` difficulty.
//!
//! The Blend difficulty is retargeted from the *average number of
//! transactions per block* over a whole epoch. An epoch's totals are only
//! usable once the epoch has ended, and only once its last blocks are deep
//! enough to be beyond re-org: they are therefore closed at the epoch
//! boundary and consumed later, at the nonce snapshot slot of the epoch after
//! that.

use serde::{Deserialize, Serialize};

/// The rolling block and transaction counts of the epoch being extended and of
/// the last epoch that closed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxDensity {
    /// Totals of the epoch the ledger is currently extending, still growing
    /// with every applied block.
    current_epoch: EpochTotals,
    /// Totals of the most recently closed epoch, final and no longer moving.
    ///
    /// `None` until the first epoch closes: before that no complete epoch
    /// exists to read a load from, which is a different state from an epoch
    /// that closed having carried nothing.
    last_closed_epoch: Option<EpochTotals>,
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
    /// block was produced in them) close as empty and are read as no load.
    pub(super) const fn close_epoch(&mut self) {
        self.last_closed_epoch = Some(self.current_epoch);
        self.current_epoch = EpochTotals { blocks: 0, txs: 0 };
    }

    /// The load of the last closed epoch — the observation the Blend
    /// difficulty retarget reads — or `None` while no epoch has closed yet.
    pub(crate) const fn last_closed_epoch_load(&self) -> Option<ClosedEpochLoad> {
        match self.last_closed_epoch {
            Some(EpochTotals { blocks, txs }) => Some(ClosedEpochLoad {
                transactions: txs,
                blocks,
            }),
            None => None,
        }
    }
}

/// The transaction load of a single epoch that has *closed*.
///
/// The retarget must never read an epoch that is still being extended — its
/// totals would still be growing, and a difficulty derived from them would
/// depend on when it was read. Only [`TxDensity::close_epoch`] can put an
/// epoch's counts into this type, so passing an open epoch to the controller
/// does not compile rather than silently mis-targeting the difficulty. Naming
/// the two counts also keeps them from being transposed at the call site,
/// where as bare integers they are indistinguishable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClosedEpochLoad {
    transactions: u64,
    blocks: u64,
}

impl ClosedEpochLoad {
    #[cfg(test)]
    pub const fn new(transactions: u64, blocks: u64) -> Self {
        Self {
            transactions,
            blocks,
        }
    }
    /// Transactions carried by the whole epoch.
    pub const fn transactions(self) -> u64 {
        self.transactions
    }

    /// Blocks the epoch produced. Zero for an epoch that was skipped entirely.
    pub const fn blocks(self) -> u64 {
        self.blocks
    }
}

/// Totals accumulated over a single epoch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals_accumulate_until_the_epoch_closes() {
        let mut density = TxDensity::default();
        assert_eq!(density.last_closed_epoch_load(), None);

        density.record_block(3);
        density.record_block(0);
        density.record_block(7);
        // Still open: nothing is observable yet.
        assert_eq!(density.last_closed_epoch_load(), None);

        density.close_epoch();
        assert_eq!(
            density.last_closed_epoch_load(),
            Some(ClosedEpochLoad::new(10, 3))
        );
    }

    #[test]
    fn blocks_of_the_new_epoch_do_not_move_the_closed_totals() {
        let mut density = TxDensity::default();
        density.record_block(5);
        density.close_epoch();
        density.record_block(100);
        assert_eq!(
            density.last_closed_epoch_load(),
            Some(ClosedEpochLoad::new(5, 1))
        );
    }

    #[test]
    fn a_skipped_epoch_closes_as_empty() {
        // Two epochs crossed at once: the first close hands over the real
        // totals, the second closes the epoch that had no blocks at all.
        let mut density = TxDensity::default();
        density.record_block(5);
        density.close_epoch();
        density.close_epoch();
        assert_eq!(
            density.last_closed_epoch_load(),
            Some(ClosedEpochLoad::new(0, 0))
        );
    }
}
