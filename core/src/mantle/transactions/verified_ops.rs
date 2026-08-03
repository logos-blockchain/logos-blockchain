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

impl<'tx> From<&'tx SignedMantleTx<Preverified>> for VerifiedOps<'tx> {
    fn from(transaction: &'tx SignedMantleTx<Preverified>) -> Self {
        VerifiedOps::new(transaction)
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::Fr;
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey};
    use num_bigint::BigUint;

    use crate::mantle::{
        Note, Op, OpProof, SignedMantleTx, Utxo, VerificationError,
        channel::Channels,
        ledger::{Inputs, Outputs, OutputsError},
        ops::{
            channel::{ChannelId, config::Keys},
            transfer::{TransferError, TransferOp},
        },
        traits::Hashable as _,
        transactions::{
            signed_mantle_tx::test_utils::{
                create_test_mantle_tx, create_withdraw_tx, make_channel_state,
            },
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
            let mut channels = Channels::new();
            let channel_state = make_channel_state(2, Some(keys));
            channels.channels.insert_mut(channel_id, channel_state);
            channels
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

    #[test]
    fn helper_backed_verification_rejects_zero_value_transfer_output() {
        let input_sk = ZkKey::from(BigUint::from(1u8));
        let input_utxo = Utxo {
            op_id: [1u8; 32],
            output_index: 0,
            note: Note::new(10000, input_sk.to_public_key()),
        };

        let signed_tx = {
            let transfer_op = TransferOp::new(
                Inputs::new([input_utxo.id()]),
                Outputs::new([Note::new(0, Fr::from(BigUint::from(2u8)).into())]),
            );
            let mantle_tx = create_test_mantle_tx(vec![Op::Transfer(transfer_op)]);
            let transfer_sig = ZkKey::multi_sign(&[input_sk], &mantle_tx.hash().to_fr())
                .expect("Signing should succeed");
            SignedMantleTx::new(mantle_tx, [OpProof::ZkSig(transfer_sig)].into())
                .preverify()
                .expect("Transfer transaction should preverify")
        };

        let helper =
            TestOperationVerificationHelper::new(Channels::new(), []).with_utxos([input_utxo]);

        let verification_result = signed_tx
            .verified_ops()
            .next(&helper)
            .expect("Cursor should yield the TransferOp");
        assert_eq!(
            verification_result,
            Err(VerificationError::TransferVerificationError(
                TransferError::Outputs(OutputsError::ZeroValueNote)
            ))
        );
    }

    #[test]
    fn helper_backed_verification_rejects_missing_channel() {
        let channel_id = ChannelId::from([10u8; 32]);
        let key0 = Ed25519Key::from_bytes(&[0; 32]);
        let signed_tx = create_withdraw_tx(channel_id, &[&key0], None);

        let channels = Channels::new();
        let helper = TestOperationVerificationHelper::new(channels, []);

        let verification_result = signed_tx.verified_ops().next(&helper).unwrap();
        assert_eq!(
            verification_result,
            Err(VerificationError::ChannelNotFound { channel_id })
        );
    }

    #[test]
    fn helper_backed_verification_rejects_missing_key() {
        let channel_id = ChannelId::from([10u8; 32]);
        let key0 = Ed25519Key::from_bytes(&[0; 32]);
        let key1 = Ed25519Key::from_bytes(&[1; 32]);
        let signed_tx = create_withdraw_tx(channel_id, &[&key0, &key1], None);

        let channels = {
            let mut channels = Channels::new();
            let channel_state = make_channel_state(2, None);
            channels.channels.insert_mut(channel_id, channel_state);
            channels
        };
        let helper =
            TestOperationVerificationHelper::new(channels, [((channel_id, 0), key0.public_key())]);

        let verification_result = signed_tx.verified_ops().next(&helper).unwrap();
        assert_eq!(
            verification_result,
            Err(VerificationError::KeyNotFound {
                channel_id,
                key_index: 1
            })
        );
    }

    #[test]
    fn helper_backed_verification_rejects_not_enough_signatures() {
        let channel_id = ChannelId::from([10u8; 32]);
        let key0 = Ed25519Key::from_bytes(&[0; 32]);
        let signed_tx = create_withdraw_tx(channel_id, &[&key0], None);

        let channels = {
            let mut channels = Channels::new();
            let channel_state = make_channel_state(2, None);
            channels.channels.insert_mut(channel_id, channel_state);
            channels
        };
        let helper =
            TestOperationVerificationHelper::new(channels, [((channel_id, 0), key0.public_key())]);

        let verification_result = signed_tx.verified_ops().next(&helper).unwrap();
        assert_eq!(
            verification_result,
            Err(VerificationError::ChannelMultiSigProofNotEnoughSignatures {
                op_index: 0,
                actual: 1,
                required: 2
            })
        );
    }

    #[test]
    fn helper_backed_verification_rejects_invalid_signature() {
        let channel_id = ChannelId::from([10u8; 32]);
        let expected_key = Ed25519Key::from_bytes(&[0; 32]);
        let wrong_key = Ed25519Key::from_bytes(&[9; 32]);
        let signed_tx = create_withdraw_tx(channel_id, &[&wrong_key], None);

        let channels = {
            let mut channels = Channels::new();
            let channel_state = make_channel_state(1, None);
            channels.channels.insert_mut(channel_id, channel_state);
            channels
        };
        let helper = TestOperationVerificationHelper::new(
            channels,
            [((channel_id, 0), expected_key.public_key())],
        );

        let verification_result = signed_tx.verified_ops().next(&helper).unwrap();
        assert_eq!(
            verification_result,
            Err(VerificationError::ChannelMultiSigProofInvalidSignature {
                op_index: 0,
                signature_index: 0
            })
        );
    }
}
