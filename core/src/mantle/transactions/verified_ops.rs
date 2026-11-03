use std::{iter::Enumerate, vec::IntoIter};

use crate::mantle::{
    VerificationError,
    batch::DeferredProofs,
    ledger::verification_mode::StandardMode,
    ops::SignedOp,
    traits::Hashable as _,
    transactions::{
        OperationVerificationHelper, SignedOps,
        hash::TxHashView,
        states::{Preverified, Verified},
    },
};

pub type VerifiedOperationStep = (VerifiedOperations, SignedOp<Verified, StandardMode>);

pub struct VerifiedOperations {
    signed_ops: Enumerate<IntoIter<SignedOp<Preverified, StandardMode>>>,
    tx_hash_view: TxHashView,
}

impl VerifiedOperations {
    #[must_use]
    pub fn new(signed_ops: SignedOps<Preverified, StandardMode>) -> Self {
        let tx_hash = signed_ops.hash();
        let tx_hash_view = TxHashView::from(tx_hash);
        let signed_ops = signed_ops.into_iter().enumerate();
        Self {
            signed_ops,
            tx_hash_view,
        }
    }

    /// Yields the next operation if it passes verification.
    ///
    /// Verification by spec is linear:
    /// each operation is checked against the state its predecessors produced.
    ///
    /// The cursor is handed back only on success, so a failure ends the
    /// sequence by construction: there is no way to verify an operation
    /// against a state its predecessor never contributed to.
    ///
    /// # Parameters
    ///
    /// - `helper`: Provider of verification context. It must reflect every
    ///   preceding operation in this transaction.
    /// - `deferred_proofs`: Batch that receives the operation's deferred proof,
    ///   if it has one. Only updated on verification success.
    ///
    /// # Returns
    ///
    /// - `Some(Ok(VerifiedOperationStep))` if the next operation is
    ///   successfully verified.
    /// - `Some(Err(error))` if the next operation fails verification.
    /// - `None` if there are no more operations to verify.
    ///
    /// # Errors
    ///
    /// Returns [`VerificationError`] if the operation at the current index
    /// fails verification.
    pub fn next(
        mut self,
        helper: &impl OperationVerificationHelper,
        deferred_proofs: &mut DeferredProofs,
    ) -> Option<Result<VerifiedOperationStep, VerificationError>> {
        let (index, signed_op) = self.signed_ops.next()?;
        let verify_result =
            signed_op.into_verified(index, &self.tx_hash_view, helper, deferred_proofs);

        Some(
            verify_result
                .map(|verified_operation| (self, verified_operation))
                .map_err(|(_signed_op, error)| error),
        )
    }

    #[must_use]
    pub const fn tx_hash_view(&self) -> &TxHashView {
        &self.tx_hash_view
    }
}

impl From<SignedOps<Preverified, StandardMode>> for VerifiedOperations {
    fn from(signed_ops: SignedOps<Preverified, StandardMode>) -> Self {
        Self::new(signed_ops)
    }
}

#[cfg(test)]
mod tests {
    use lb_cryptarchia_engine::Slot;
    use lb_groth16::Fr;
    use lb_key_management_system_keys::keys::Ed25519Key;

    use crate::mantle::{
        Op, OpProof,
        batch::DeferredProofs,
        channel::Channels,
        ledger::verification_mode::StandardMode,
        note_set::NoteSets,
        notes::{NoteNullifier, OutputCommitments},
        ops::{
            ZkTransferProof,
            channel::{ChannelId, withdraw::ChannelWithdrawOp},
            op_proof::samples::SampleProof as _,
        },
        traits::Hashable as _,
        transactions::{
            OpProofs, Ops, SignedOps, states::Preverified,
            verification_helper::test_utils::TestOperationVerificationHelper,
        },
    };

    fn valid_withdraw() -> (
        SignedOps<Preverified, StandardMode>,
        TestOperationVerificationHelper,
    ) {
        let channel_id = ChannelId::from([8u8; 32]);
        let keys = [Ed25519Key::from_bytes(&[8; 32])
            .public_key()
            .into_unverified()]
        .into();
        let (channels, note_sets) =
            Channels::new().create_channel(channel_id, Slot::from(0u64), keys, NoteSets::new());
        let note_sets = note_sets.end_block(0);
        let commitment_merkle_root = note_sets
            .get(2)
            .expect("the channel holds note set 2")
            .commitment_root();

        let ops = Ops::from([Op::ChannelWithdraw(ChannelWithdrawOp {
            channel_id,
            inputs: [NoteNullifier(Fr::from(1u64))].into(),
            commitment_merkle_root,
            outputs: OutputCommitments::default(),
        })]);
        let signed_tx = SignedOps::from_parts(
            ops,
            OpProofs::from([OpProof::ZkTransfer(ZkTransferProof::sample())]),
        )
        .unwrap()
        .preverify()
        .unwrap();

        let helper = TestOperationVerificationHelper::new(channels, [])
            .with_note_sets(note_sets)
            .with_block_number(1);

        (signed_tx, helper)
    }

    #[test]
    fn next_returns_none_once_the_operations_are_exhausted() {
        let (signed_tx, helper) = valid_withdraw();
        let verified_ops = signed_tx.into_verified();

        let mut deferred_proofs = DeferredProofs::new();
        let (verified_ops, _) = verified_ops
            .next(&helper, &mut deferred_proofs)
            .expect("Cursor should yield the WithdrawOp")
            .expect("WithdrawOp should verify");

        assert!(verified_ops.next(&helper, &mut deferred_proofs).is_none());
    }

    #[test]
    fn tx_hash_view_carries_the_transaction_hash() {
        let (signed_tx, _helper) = valid_withdraw();
        let tx_hash = signed_tx.hash();

        let verified_ops = signed_tx.into_verified();

        assert_eq!(verified_ops.tx_hash_view().tx_hash(), &tx_hash);
    }
}
