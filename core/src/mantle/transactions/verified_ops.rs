use bytes::Bytes;
use lb_groth16::Fr;

use crate::mantle::{
    Op, OpProof, SignedMantleTx, TxHash, VerificationError,
    traits::Hashable as _,
    transactions::{OperationVerificationHelper, states::Preverified},
};

pub struct VerifiedOps<'tx> {
    ops: &'tx [Op],
    proofs: &'tx [OpProof],
    tx_hash: TxHash,
    tx_hash_fr: Fr,
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
            tx_hash_fr: tx_hash.to_fr(),
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
            &self.tx_hash_fr,
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

impl<'tx> From<&'tx SignedMantleTx<Preverified>> for VerifiedOps<'tx> {
    fn from(transaction: &'tx SignedMantleTx<Preverified>) -> Self {
        VerifiedOps::new(transaction)
    }
}

#[cfg(test)]
mod tests {
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey};
    use num_bigint::BigUint;

    use crate::mantle::{
        Note, Utxo, VerificationError,
        channel::{Channels, Error},
        ledger::Inputs,
        ops::channel::{ChannelId, config::Keys},
        transactions::{
            signed_mantle_tx::test_utils::{create_withdraw_tx, make_channel_state},
            verification_helper::test_utils::TestOperationVerificationHelper,
        },
    };

    #[test]
    fn helper_backed_verification_accepts_valid_channel_withdraw() {
        let channel_id = ChannelId::from([8u8; 32]);
        let key0 = Ed25519Key::from_bytes(&[8; 32]);
        let key1 = Ed25519Key::from_bytes(&[9; 32]);
        let keys = Keys::new_unchecked(vec![key0.public_key(), key1.public_key()]);

        let input_sk = ZkKey::from(BigUint::from(1u8));
        let utxo = Utxo {
            op_id: [1u8; 32],
            output_index: 0,
            note: Note::new(10, input_sk.to_public_key()),
        };
        let note_id = utxo.id();
        let withdraw_inputs = Inputs::from([note_id]);

        let signed_tx = create_withdraw_tx(channel_id, &[&key0, &key1], Some(withdraw_inputs));

        let channels = {
            let channels = Channels::new();
            let channel_state = make_channel_state(2, Some(keys));
            channels
                .set_channel_state(&channel_id, channel_state)
                .register_channel_note(&note_id, &channel_id)
                .expect("Note should be registered.")
        };

        let helper = TestOperationVerificationHelper::new(
            channels,
            [
                ((channel_id, 0), key0.public_key()),
                ((channel_id, 1), key1.public_key()),
            ],
        )
        .with_utxos(vec![utxo]);

        signed_tx
            .verified_ops()
            .next(&helper)
            .expect("Cursor should yield the WithdrawOp")
            .expect("WithdrawOp should verify");
    }

    /// Only checks that a failing op's error reaches the cursor unchanged.
    /// The specific reasons a channel-withdraw proof can be rejected (missing
    /// channel/key, threshold, bad signature) are covered where that logic
    /// lives, in `ops::channel::verification::tests`.
    #[test]
    fn helper_backed_verification_surfaces_op_validation_error() {
        let channel_id = ChannelId::from([10u8; 32]);
        let key0 = Ed25519Key::from_bytes(&[0; 32]);
        let signed_tx = create_withdraw_tx(channel_id, &[&key0], None);

        let helper = TestOperationVerificationHelper::new(Channels::new(), []);

        let verification_result = signed_tx.verified_ops().next(&helper).unwrap();
        assert_eq!(
            verification_result,
            Err(VerificationError::ChannelVerificationError(
                Error::InvalidSignature
            ))
        );
    }
}
