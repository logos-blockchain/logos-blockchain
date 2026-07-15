use crate::mantle::{
    GasCalculator, MantleTx, Op, OpProof,
    traits::{Hashable, StorageSize},
    transactions::hash::TxHash,
};

pub type OpWithProof<'a> = (&'a Op, &'a OpProof);

pub trait MantleTxWithProofs: Hashable<Hash = TxHash> + GasCalculator + StorageSize {
    /// Returns the underlying `MantleTx` that this transaction represents.
    fn mantle_tx(&self) -> &MantleTx;

    /// Returns an iterator over the operations and their corresponding proofs
    /// in this transaction.
    fn ops_with_proof(&self) -> impl Iterator<Item = OpWithProof<'_>>;
}

impl<T: MantleTxWithProofs> MantleTxWithProofs for &T {
    fn mantle_tx(&self) -> &MantleTx {
        T::mantle_tx(self)
    }

    fn ops_with_proof(&self) -> impl Iterator<Item = OpWithProof<'_>> {
        T::ops_with_proof(self)
    }
}
