use crate::mantle::{traits::mantle_tx::MantleTxWithProofs, transactions::VerifiedOps};

pub trait PreverifiedMantleTx: MantleTxWithProofs {
    /// Returns the cursor to the verified operations in this transaction.
    fn verified_ops(&self) -> VerifiedOps<'_>;
}
