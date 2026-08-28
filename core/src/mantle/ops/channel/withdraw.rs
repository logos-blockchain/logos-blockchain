use lb_codec::{BinaryCodec, BinaryEncode as _};
#[cfg(any(test, feature = "samples"))]
use lb_groth16::Fr;
use serde::{Deserialize, Serialize};

#[cfg(any(test, feature = "samples"))]
use crate::mantle::NoteId;
use crate::{
    events::TxEvent,
    mantle::{
        TxHash, Value,
        channel::{Channels, Error},
        gas::{Gas, MainnetGasProfile, OperationGas, SignedOperationExecutionGas},
        ledger::{
            ExecutableOperation, Inputs, PreverifiableOperation, ProvableOperation, Utxos,
            VerifiableOperation,
            verification_mode::{StandardMode, VerificationMode},
        },
        ops::{
            OpId, SignedOperation,
            channel::{ChannelId, verification::verify_channel_multi_sig},
        },
        transactions::{
            OperationVerificationHelper,
            hash::TxHashView,
            states::{Preverified, Unverified, VerificationState, Verified},
        },
    },
    proofs::channel_multi_sig_proof::ChannelMultiSigProof,
    sdp::locked_notes::LockedNotes,
};

// ChannelWithdraw = ChannelId Inputs — plain field-order concat.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, BinaryCodec)]
pub struct ChannelWithdrawOp {
    pub channel_id: ChannelId,
    pub inputs: Inputs,
}

impl OpId for ChannelWithdrawOp {
    fn op_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }
}

pub struct WithdrawValidationContext<'a> {
    pub channels: &'a Channels,
    pub locked_notes: &'a LockedNotes,
    pub utxos: &'a Utxos,
    pub tx_hash_view: &'a TxHashView,
    pub op_index: usize,
    pub helper: &'a dyn OperationVerificationHelper,
}

#[derive(Debug, PartialEq, Eq)]
pub struct WithdrawExecutionContext {
    pub channels: Channels,
    pub tx_hash: TxHash,
}

impl ProvableOperation for ChannelWithdrawOp {
    // `SignedOperationExecutionGas::gas_multiplier` below reads this proof's
    // signature count. If this changes, update that too.
    type Proof = ChannelMultiSigProof;
    const CODE: u8 = 0x13;
}

impl OperationGas<MainnetGasProfile> for ChannelWithdrawOp {
    const GAS_COST: Gas = Gas::new(56);
}

impl PreverifiableOperation<StandardMode>
    for SignedOperation<ChannelWithdrawOp, Unverified, StandardMode>
{
    type Context<'a> = ();
    type Error = Error;

    fn preverify(&self, _context: &Self::Context<'_>) -> Result<(), Self::Error> {
        // Ensure the inputs is non-empty
        self.operation().inputs.preverify()?;

        Ok(())
    }
}

impl VerifiableOperation<StandardMode>
    for SignedOperation<ChannelWithdrawOp, Preverified, StandardMode>
{
    type Context<'a> = WithdrawValidationContext<'a>;
    type Error = Error;

    fn verify(&self, context: &Self::Context<'_>) -> Result<(), Self::Error> {
        let operation = self.operation();
        let proof = self.proof();

        verify_channel_multi_sig(
            &operation.channel_id,
            proof,
            context.tx_hash_view.as_bytes(),
            context.helper,
            context.op_index,
        )
        .map_err(|_error| Error::InvalidSignature)?; // FIXME: Discards error details

        // Check that the channel exists
        let channel =
            context
                .channels
                .channels
                .get(&operation.channel_id)
                .ok_or(Error::ChannelNotFound {
                    channel_id: operation.channel_id,
                })?;

        // Check that the inputs are valid and belong to the channel
        operation.inputs.validate_in_channel(
            context.locked_notes,
            context.channels,
            &operation.channel_id,
            context.utxos,
        )?;

        // Check there is enough signatures
        let signatures = proof.signatures();
        if signatures.len() != channel.transfer_threshold as usize {
            return Err(Error::ThresholdUnmet {
                channel_id: operation.channel_id,
                threshold: channel.transfer_threshold,
                actual: signatures.len(),
            });
        }

        // Check the signatures
        for sig in signatures {
            if channel
                .accredited_keys
                .get(sig.channel_key_index as usize)
                .ok_or(Error::InvalidSignature)?
                .verify(context.tx_hash_view.as_bytes(), &sig.signature)
                .is_err()
            {
                return Err(Error::InvalidSignature);
            }
        }

        Ok(())
    }
}

impl<Mode: VerificationMode> ExecutableOperation
    for SignedOperation<ChannelWithdrawOp, Verified, Mode>
{
    type Context<'a> = WithdrawExecutionContext;
    type Error = Error;

    fn execute<'a>(
        &self,
        mut context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        let operation = self.operation();

        // Release the inputs from the channel. The notes keep their NoteId,
        // value and ZkPublicKey and stay in the ledger as regular notes.
        for note_id in operation.inputs.iter() {
            context.channels = context
                .channels
                .unregister_channel_note(note_id, &operation.channel_id)?;
        }

        Ok((context, Vec::new()))
    }
}

impl<State: VerificationState, Mode: VerificationMode> SignedOperationExecutionGas
    for SignedOperation<ChannelWithdrawOp, State, Mode>
{
    fn gas_multiplier(&self) -> Value {
        let signature_count = self.proof().signatures().len();
        Value::try_from(signature_count)
            .expect("Channel multi-signature proofs are bound to u16::MAX signatures.")
    }
}

#[cfg(any(test, feature = "samples"))]
impl ChannelWithdrawOp {
    #[must_use]
    pub fn sample() -> Self {
        Self {
            channel_id: ChannelId::from([18u8; 32]),
            inputs: Inputs::new([NoteId(Fr::from(19u64))]),
        }
    }
}

#[cfg(test)]
mod test {
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkPublicKey};

    use super::*;
    use crate::mantle::{
        Note, Utxo, channel_notes,
        ledger::InputsError,
        ops::channel::{config::Keys, verification::test_utils::create_channel_multi_sig_proof},
        transactions::{
            tx_list::signed_ops::test_utils::make_channel_state,
            verification_helper::test_utils::TestOperationVerificationHelper,
        },
    };

    const CHANNEL_ID: ChannelId = ChannelId([18u8; 32]);

    fn signing_key() -> Ed25519Key {
        Ed25519Key::from_bytes(&[0; 32])
    }

    fn utxo() -> Utxo {
        Utxo {
            op_id: [1u8; 32],
            output_index: 0,
            note: Note::new(10_000, ZkPublicKey::from(Fr::from(1u64))),
        }
    }

    fn channel_view(register_note: bool) -> Channels {
        let mut channels = Channels::new();
        channels.channels.insert_mut(
            CHANNEL_ID,
            make_channel_state(
                1,
                Some(Keys::new_unchecked(vec![signing_key().public_key()])),
            ),
        );

        if register_note {
            channels
                .register_channel_note(&utxo().id(), &CHANNEL_ID)
                .expect("the note is not owned by another channel")
        } else {
            channels
        }
    }

    fn ledger_view(transfer_threshold: u16, accredited_keys: Keys) -> Channels {
        let mut channels = Channels::new();
        channels.channels.insert_mut(
            CHANNEL_ID,
            make_channel_state(transfer_threshold, Some(accredited_keys)),
        );

        channels
            .register_channel_note(&utxo().id(), &CHANNEL_ID)
            .expect("the note is not owned by another channel")
    }

    fn preverified(
        proof: ChannelMultiSigProof,
    ) -> SignedOperation<ChannelWithdrawOp, Preverified, StandardMode> {
        let operation = ChannelWithdrawOp {
            channel_id: CHANNEL_ID,
            inputs: Inputs::new([utxo().id()]),
        };

        SignedOperation::<_, Unverified, StandardMode>::new(operation, proof)
            .into_preverified(&())
            .expect("preverify accepts a non-empty input list")
    }

    #[test]
    fn preverify_rejects_empty_inputs() {
        let withdraw = ChannelWithdrawOp {
            channel_id: ChannelId::from([0u8; 32]),
            inputs: Inputs::empty(),
        };
        let proof = ChannelMultiSigProof::try_new([].into()).unwrap();
        let signed_operation = SignedOperation::new(withdraw, proof);

        assert_eq!(
            signed_operation.preverify(&()),
            Err(Error::Inputs(InputsError::EmptyInputs))
        );
    }

    #[test]
    fn verify_rejects_signatures_over_another_transaction() {
        let signed_hash = TxHash::from([9u8; 32]);
        let other_hash = TxHash::from([10u8; 32]);
        let signed_operation = preverified(create_channel_multi_sig_proof(
            &signed_hash,
            &[&signing_key()],
        ));

        let channels = channel_view(true);
        let helper = TestOperationVerificationHelper::new(
            channel_view(true),
            [((CHANNEL_ID, 0), signing_key().public_key())],
        );
        let locked_notes = LockedNotes::new();
        let (utxos, _) = Utxos::new().insert(utxo().id(), utxo());

        assert_eq!(
            signed_operation.verify(&WithdrawValidationContext {
                channels: &channels,
                locked_notes: &locked_notes,
                utxos: &utxos,
                tx_hash_view: &TxHashView::from(signed_hash),
                op_index: 0,
                helper: &helper,
            }),
            Ok(())
        );
        assert_eq!(
            signed_operation.verify(&WithdrawValidationContext {
                channels: &channels,
                locked_notes: &locked_notes,
                utxos: &utxos,
                tx_hash_view: &TxHashView::from(other_hash),
                op_index: 0,
                helper: &helper,
            }),
            Err(Error::InvalidSignature)
        );
    }

    #[test]
    fn verify_rejects_a_signature_the_ledger_view_key_does_not_match() {
        let signed_hash = TxHash::from([9u8; 32]);
        let signed_operation = preverified(create_channel_multi_sig_proof(
            &signed_hash,
            &[&signing_key()],
        ));

        let channels = ledger_view(
            1,
            Keys::new_unchecked(vec![Ed25519Key::from_bytes(&[1; 32]).public_key()]),
        );
        let helper = TestOperationVerificationHelper::new(
            channel_view(true),
            [((CHANNEL_ID, 0), signing_key().public_key())],
        );
        let locked_notes = LockedNotes::new();
        let (utxos, _) = Utxos::new().insert(utxo().id(), utxo());

        assert_eq!(
            signed_operation.verify(&WithdrawValidationContext {
                channels: &channels,
                locked_notes: &locked_notes,
                utxos: &utxos,
                tx_hash_view: &TxHashView::from(signed_hash),
                op_index: 0,
                helper: &helper,
            }),
            Err(Error::InvalidSignature)
        );
    }

    #[test]
    fn verify_rejects_a_signature_index_the_ledger_view_cannot_resolve() {
        let signed_hash = TxHash::from([9u8; 32]);
        let signed_operation = preverified(create_channel_multi_sig_proof(
            &signed_hash,
            &[&signing_key()],
        ));

        let channels = ledger_view(1, Keys::new_unchecked(vec![]));
        let helper = TestOperationVerificationHelper::new(
            channel_view(true),
            [((CHANNEL_ID, 0), signing_key().public_key())],
        );
        let locked_notes = LockedNotes::new();
        let (utxos, _) = Utxos::new().insert(utxo().id(), utxo());

        assert_eq!(
            signed_operation.verify(&WithdrawValidationContext {
                channels: &channels,
                locked_notes: &locked_notes,
                utxos: &utxos,
                tx_hash_view: &TxHashView::from(signed_hash),
                op_index: 0,
                helper: &helper,
            }),
            Err(Error::InvalidSignature)
        );
    }

    #[test]
    fn verify_rejects_a_channel_missing_from_the_ledger_view() {
        let signed_hash = TxHash::from([9u8; 32]);
        let signed_operation = preverified(create_channel_multi_sig_proof(
            &signed_hash,
            &[&signing_key()],
        ));

        let helper = TestOperationVerificationHelper::new(
            channel_view(true),
            [((CHANNEL_ID, 0), signing_key().public_key())],
        );
        let locked_notes = LockedNotes::new();
        let (utxos, _) = Utxos::new().insert(utxo().id(), utxo());

        assert_eq!(
            signed_operation.verify(&WithdrawValidationContext {
                channels: &Channels::new(),
                locked_notes: &locked_notes,
                utxos: &utxos,
                tx_hash_view: &TxHashView::from(signed_hash),
                op_index: 0,
                helper: &helper,
            }),
            Err(Error::ChannelNotFound {
                channel_id: CHANNEL_ID
            })
        );
    }

    #[test]
    fn verify_rejects_an_input_the_channel_does_not_own() {
        let signed_hash = TxHash::from([9u8; 32]);
        let signed_operation = preverified(create_channel_multi_sig_proof(
            &signed_hash,
            &[&signing_key()],
        ));

        let channels = channel_view(false);
        let helper = TestOperationVerificationHelper::new(
            channel_view(false),
            [((CHANNEL_ID, 0), signing_key().public_key())],
        );
        let locked_notes = LockedNotes::new();
        let (utxos, _) = Utxos::new().insert(utxo().id(), utxo());

        assert_eq!(
            signed_operation.verify(&WithdrawValidationContext {
                channels: &channels,
                locked_notes: &locked_notes,
                utxos: &utxos,
                tx_hash_view: &TxHashView::from(signed_hash),
                op_index: 0,
                helper: &helper,
            }),
            Err(Error::Inputs(InputsError::NotAChannelNote(utxo().id())))
        );
    }

    #[test]
    fn verify_rejects_a_signature_count_below_the_ledger_view_threshold() {
        let signed_hash = TxHash::from([9u8; 32]);
        let signed_operation = preverified(create_channel_multi_sig_proof(
            &signed_hash,
            &[&signing_key()],
        ));

        let channels = ledger_view(2, Keys::new_unchecked(vec![signing_key().public_key()]));
        let helper = TestOperationVerificationHelper::new(
            channel_view(true),
            [((CHANNEL_ID, 0), signing_key().public_key())],
        );
        let locked_notes = LockedNotes::new();
        let (utxos, _) = Utxos::new().insert(utxo().id(), utxo());

        assert_eq!(
            signed_operation.verify(&WithdrawValidationContext {
                channels: &channels,
                locked_notes: &locked_notes,
                utxos: &utxos,
                tx_hash_view: &TxHashView::from(signed_hash),
                op_index: 0,
                helper: &helper,
            }),
            Err(Error::ThresholdUnmet {
                channel_id: CHANNEL_ID,
                threshold: 2,
                actual: 1,
            })
        );
    }

    fn verified() -> SignedOperation<ChannelWithdrawOp, Verified, StandardMode> {
        SignedOperation::<_, Unverified, StandardMode>::new(
            ChannelWithdrawOp {
                channel_id: CHANNEL_ID,
                inputs: Inputs::new([utxo().id()]),
            },
            ChannelMultiSigProof::sample_with_signatures(1),
        )
        .into_state_trusted()
    }

    #[test]
    fn execute_releases_the_inputs_from_the_channel() {
        let (context, events) = verified()
            .execute(WithdrawExecutionContext {
                channels: channel_view(true),
                tx_hash: TxHash::from([9u8; 32]),
            })
            .expect("the input is a channel note of this channel");

        assert!(!context.channels.is_channel_note(&utxo().id()));
        assert!(events.is_empty());
    }

    #[test]
    fn execute_rejects_an_input_no_channel_owns() {
        assert_eq!(
            verified()
                .execute(WithdrawExecutionContext {
                    channels: channel_view(false),
                    tx_hash: TxHash::from([9u8; 32]),
                })
                .map(|_| ())
                .map_err(|(_, error)| error),
            Err(Error::ChannelNotes(channel_notes::Error::NotInChannel(
                utxo().id()
            )))
        );
    }

    #[test]
    fn execute_rejects_an_input_another_channel_owns() {
        let channels = channel_view(false)
            .register_channel_note(&utxo().id(), &ChannelId::from([19u8; 32]))
            .expect("the note is not owned by another channel yet");

        assert_eq!(
            verified()
                .execute(WithdrawExecutionContext {
                    channels,
                    tx_hash: TxHash::from([9u8; 32]),
                })
                .map(|_| ())
                .map_err(|(_, error)| error),
            Err(Error::ChannelNotes(channel_notes::Error::NotAChannelNote {
                note_id: utxo().id(),
                channel_id: CHANNEL_ID,
            }))
        );
    }

    #[test]
    fn channel_withdraw_op_execution_gas() {
        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
            ChannelWithdrawOp::sample(),
            ChannelMultiSigProof::sample_with_signatures(3),
        );

        assert_eq!(
            signed_operation.execution_gas::<MainnetGasProfile>(),
            Ok(Gas::new(168))
        );
    }
}
