use bytes::Bytes;

use crate::mantle::{
    Op, OpProof, SignedMantleTx, TxHash, VerificationError,
    traits::Hashable as _,
    transactions::{OperationVerificationHelper, states::Preverified},
};

pub struct VerifiedOps<'tx> {
    ops: &'tx [Op],
    proofs: &'tx [OpProof],
    tx_hash: TxHash,
    tx_hash_bytes: Bytes,
    index: usize,
}

impl<'tx> VerifiedOps<'tx> {
    #[must_use]
    pub fn new(transaction: &'tx SignedMantleTx<Preverified>) -> Self {
        let ops = transaction.mantle_tx.ops();
        let proofs = transaction.ops_proofs();
        let tx_hash = transaction.hash();
        Self {
            ops,
            proofs,
            tx_hash,
            tx_hash_bytes: tx_hash.as_signing_bytes(),
            index: 0,
        }
    }

    /// Yields the next operation, in order, if it passes verification.
    ///
    /// # Returns
    ///
    /// - `Some(Ok(op))` if the next operation is successfully verified.
    /// - `Some(Err(error))` if the next operation fails verification.
    /// - `None` if there are no more operations to verify.
    ///
    /// # Errors
    ///
    /// Returns [`VerificationError`] if the operation at the current index
    /// fails verification. On error, the cursor is not advanced. In the
    /// current implementation, the callers are expected to abort since only
    /// linear verification is supported.
    pub fn next(
        &mut self,
        helper: &impl OperationVerificationHelper,
    ) -> Option<Result<&'tx Op, VerificationError>> {
        let index = self.index;
        let op = self.ops.get(index)?;
        let proof = self
            .proofs
            .get(index)
            .expect("SignedMantleTx<Preverified> invariant: ops and proofs have the same length");
        if let Err(error) = SignedMantleTx::<Preverified>::verify_stateful_op(
            index,
            op,
            proof,
            &self.tx_hash,
            &self.tx_hash_bytes,
            helper,
        ) {
            return Some(Err(error));
        }
        self.index += 1;
        Some(Ok(op))
    }

    #[must_use]
    pub const fn tx_hash(&self) -> &TxHash {
        &self.tx_hash
    }

    #[must_use]
    pub const fn tx_hash_bytes(&self) -> &Bytes {
        &self.tx_hash_bytes
    }
}
