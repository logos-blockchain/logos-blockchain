use crate::mantle::{
    Op, OpProof,
    traits::{Hashable, StorageSize},
    transactions::{hash::TxHash, mantle_tx::RawMantleTx},
};

pub type OpWithProof<'a> = (&'a Op, &'a OpProof);

// TODO: Supertrait to MantleTx and propagate
pub trait MantleTxWithProofs: Hashable<Hash = TxHash> + StorageSize {
    /// Returns the underlying `MantleTx` that this transaction represents.
    fn mantle_tx(&self) -> &RawMantleTx;

    /// Returns an iterator over the operations and their corresponding proofs
    /// in this transaction.
    fn ops_with_proof(&self) -> impl Iterator<Item = OpWithProof<'_>>;
}
