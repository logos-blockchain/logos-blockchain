use crate::mantle::{traits::mantle_tx::MantleTxWithProofs, transactions::tx::VerifiedOps};

pub trait PreverifiedMantleTx: MantleTxWithProofs {
    /// Returns the cursor to the verified operations in this transaction.
    fn verified_ops(&self) -> VerifiedOps<'_>;
}

impl<T: PreverifiedMantleTx> PreverifiedMantleTx for &T {
    fn verified_ops(&self) -> VerifiedOps<'_> {
        T::verified_ops(self)
    }
}
