use std::{iter::Enumerate, vec::IntoIter};

use crate::mantle::{
    VerificationError,
    ledger::verification_mode::StandardMode,
    ops::SignedOp,
    traits::Hashable as _,
    transactions::{
        OperationVerificationHelper, SignedOps,
        hash::TxHashView,
        states::{Preverified, Verified},
    },
};

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

    /// Yields the next operation, in order, if it passes verification.
    ///
    /// # Important
    ///
    /// **Callers must abort on the first error.**
    ///
    /// Verification by spec is linear: each operation is checked against the
    /// state its predecessors produced. A failed operation is still
    /// consumed, so calling this again verifies the *next* one against a
    /// state that the failed one never contributed to, which can wrongly
    /// succeed.
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
    /// fails verification.
    pub fn next(
        &mut self,
        helper: &impl OperationVerificationHelper,
    ) -> Option<Result<SignedOp<Verified, StandardMode>, VerificationError>> {
        let (index, signed_op) = self.signed_ops.next()?;
        let verify_result = signed_op.into_verified(index, &self.tx_hash_view, helper);
        Some(verify_result.map_err(|(_signed_op, error)| error))
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
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey};
    use num_bigint::BigUint;

    use crate::mantle::{
        Note, Utxo, VerificationError,
        channel::{Channels, Error},
        ledger::Inputs,
        ops::channel::{ChannelId, config::Keys},
        transactions::{
            tx_list::signed_ops::test_utils::{create_withdraw_tx, make_channel_state},
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
            .into_verified()
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

        let verification_result = signed_tx.into_verified().next(&helper).unwrap();
        assert_eq!(
            verification_result,
            Err(VerificationError::ChannelVerificationError(
                Error::InvalidSignature
            ))
        );
    }
}
