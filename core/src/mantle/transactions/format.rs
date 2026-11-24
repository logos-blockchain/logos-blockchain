//! How a transaction is encoded, which the era it is included in decides.

use core::sync::atomic::{AtomicU64, Ordering};

use lb_cryptarchia_engine::Slot;

/// The layout of a transaction's canonical encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum TxFormat {
    /// The layout of releases 0.3.1 to 0.3.6: no fork digest, and an SDP
    /// withdrawal that names its service note.
    Legacy,
    /// The fork digest the transaction is bound to, then its operations.
    ForkBound,
}

/// The first slot whose transactions are fork-bound: the first slot of the
/// era that follows the chain's legacy era, or 0 for a chain without one.
///
/// A constant of the chain the process follows, set once as the node starts.
/// It lets a block be decoded from its bytes alone, wherever the era schedule
/// is out of reach: the block's slot, which its encoding opens with, tells
/// the layout of its transactions.
static FIRST_FORK_BOUND_SLOT: AtomicU64 = AtomicU64::new(0);

impl TxFormat {
    #[must_use]
    pub const fn is_legacy(self) -> bool {
        matches!(self, Self::Legacy)
    }

    /// Declares the slots before `slot` as those of the chain's legacy era,
    /// whose transactions are in the legacy layout.
    pub fn set_first_fork_bound_slot(slot: Slot) {
        FIRST_FORK_BOUND_SLOT.store(slot.into_inner(), Ordering::Relaxed);
    }

    /// The layout of the transactions of a block at `slot`.
    #[must_use]
    pub fn at_slot(slot: Slot) -> Self {
        if slot.into_inner() < FIRST_FORK_BOUND_SLOT.load(Ordering::Relaxed) {
            Self::Legacy
        } else {
            Self::ForkBound
        }
    }
}

/// What decoding a transaction needs to know of the era it is decoded under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxDecodeContext {
    pub format: TxFormat,
    /// The most bytes of data an Operation may carry: an inscription, or a
    /// deposit's metadata.
    pub max_op_data_size: u32,
}

impl TxDecodeContext {
    /// A fork-bound transaction, whose Operations carry any amount of data:
    /// for transactions that are not decoded under an era.
    pub const UNBOUNDED: Self = Self::fork_bound(u32::MAX);
    /// A legacy transaction, whose Operations carry any amount of data.
    pub const UNBOUNDED_LEGACY: Self = Self {
        format: TxFormat::Legacy,
        max_op_data_size: u32::MAX,
    };

    /// A transaction of a block at `slot`, in the layout of the era of that
    /// slot.
    #[must_use]
    pub fn at_slot(slot: Slot, max_op_data_size: u32) -> Self {
        Self {
            format: TxFormat::at_slot(slot),
            max_op_data_size,
        }
    }

    #[must_use]
    pub const fn fork_bound(max_op_data_size: u32) -> Self {
        Self {
            format: TxFormat::ForkBound,
            max_op_data_size,
        }
    }

    /// The most bytes of metadata a deposit may carry. The legacy layout
    /// bounds it only by the size of a block's transactions.
    #[must_use]
    pub const fn max_deposit_metadata_size(&self) -> u32 {
        match self.format {
            TxFormat::Legacy => u32::MAX,
            TxFormat::ForkBound => self.max_op_data_size,
        }
    }
}
