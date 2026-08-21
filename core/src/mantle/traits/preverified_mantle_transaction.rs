use crate::mantle::{traits::MantleTx, transactions::VerifiedOperations};

pub trait PreverifiedMantleTransaction: MantleTx {
    /// Returns the cursor to the verified operations in this transaction.
    fn into_verified_operations(self) -> VerifiedOperations;
}
