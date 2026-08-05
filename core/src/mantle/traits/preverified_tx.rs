use crate::mantle::{traits::mantle_tx::MantleTxWithProofs, transactions::VerifiedOperations};

pub trait PreverifiedMantleTx: MantleTxWithProofs {
    /// Returns the cursor to the verified operations in this transaction.
    fn verified_ops(&self) -> VerifiedOperations<'_>;
}
