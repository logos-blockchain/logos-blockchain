use lb_codec::{BinaryCodec, BinaryEncode as _};
#[cfg(any(test, feature = "samples"))]
use lb_groth16::Fr;
use lb_key_management_system_keys::keys::{ZkPublicKey, ZkSignature};
use lb_utils::bounded::UpperBoundedVec;
use serde::{Deserialize, Serialize};

#[cfg(any(test, feature = "samples"))]
use crate::mantle::NoteId;
use crate::{
    events::{DepositNote, DepositRecreatedNotes, TxEvent, TxEventPayload},
    mantle::{
        Value,
        channel::{Channels, Error},
        gas::{Gas, MainnetGasProfile, OperationGas, SignedOperationExecutionGas},
        ledger::{
            ExecutableOperation, Inputs, InputsError, Outputs, PreverifiableOperation,
            ProvableOperation, Utxos, VerifiableOperation,
            verification_mode::{StandardMode, VerificationMode},
        },
        ops::{OpId, SignedOperation, channel::ChannelId},
        transactions::{
            hash::{TxHash, TxHashView},
            states::{Preverified, Unverified, VerificationState, Verified},
        },
    },
    sdp::locked_notes::LockedNotes,
};

pub const MAX_METADATA_SIZE: usize = u32::MAX as usize;
pub type Metadata = UpperBoundedVec<u8, { MAX_METADATA_SIZE }>;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct DepositOp {
    pub channel_id: ChannelId,
    pub inputs: Inputs,
    pub metadata: Metadata,
}

impl DepositOp {
    // The notes re-created in the channel
    pub fn outputs(&self, utxos: &Utxos) -> Result<Outputs, Error> {
        let notes = self
            .inputs
            .iter()
            .map(|note_id| {
                utxos
                    .get(note_id)
                    .map(|utxo| utxo.note)
                    .ok_or(InputsError::InexistingNote(*note_id))
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Outputs::try_new(notes)?)
    }

    #[cfg(any(test, feature = "samples"))]
    #[must_use]
    pub fn sample() -> Self {
        Self {
            channel_id: ChannelId::from([16u8; 32]),
            inputs: Inputs::new([NoteId(Fr::from(17u64))]),
            metadata: Metadata::try_from(b"deposit-metadata".to_vec())
                .expect("Metadata is within bounds."),
        }
    }
}

impl OpId for DepositOp {
    fn op_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }
}

pub struct DepositValidationContext<'a> {
    pub channels: &'a Channels,
    pub locked_notes: &'a LockedNotes,
    pub utxos: &'a Utxos,
    pub tx_hash_view: &'a TxHashView,
}

pub struct DepositExecutionContext {
    pub channels: Channels,
    pub utxos: Utxos,
    pub tx_hash: TxHash,
}

impl ProvableOperation for DepositOp {
    type Proof = ZkSignature;
    const CODE: u8 = 0x12;
}

impl OperationGas<MainnetGasProfile> for DepositOp {
    const GAS_COST: Gas = Gas::new(590);
}

impl PreverifiableOperation<StandardMode> for SignedOperation<DepositOp, Unverified, StandardMode> {
    type Context<'a> = ();
    type Error = Error;

    fn preverify(&self, _context: &Self::Context<'_>) -> Result<(), Self::Error> {
        // Ensure the inputs is non-empty
        self.operation().inputs.preverify()?;

        Ok(())
    }
}

impl VerifiableOperation<StandardMode> for SignedOperation<DepositOp, Preverified, StandardMode> {
    type Context<'a> = DepositValidationContext<'a>;
    type Error = Error;

    fn verify(&self, context: &Self::Context<'_>) -> Result<(), Self::Error> {
        let operation = self.operation();

        // Check that the channel exists
        if !context
            .channels
            .channels
            .contains_key(&operation.channel_id)
        {
            return Err(Error::ChannelNotFound {
                channel_id: operation.channel_id,
            });
        }

        // Check that inputs are spendable and not already channel notes
        operation.inputs.validate_not_in_channel(
            context.locked_notes,
            context.channels,
            context.utxos,
        )?;

        // Check the signature
        let public_keys = operation.inputs.get_pk(context.utxos)?;
        if !ZkPublicKey::verify_multi(&public_keys, context.tx_hash_view.as_fr(), self.proof()) {
            return Err(Error::InvalidSignature);
        }

        Ok(())
    }
}

impl<Mode: VerificationMode> ExecutableOperation for SignedOperation<DepositOp, Verified, Mode> {
    type Context<'a> = DepositExecutionContext;
    type Error = Error;

    fn execute<'a>(
        &self,
        mut context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        let operation = self.operation();

        // Get the amount deposited for the event payload
        let amount_deposited = operation.inputs.amount(&context.utxos)?;
        let outputs = operation.outputs(&context.utxos)?;

        // Remove the inputs from the ledger.
        context.utxos = operation.inputs.execute(context.utxos)?;

        // Add the re-created notes to the ledger and register them as channel
        // notes.
        context.utxos = outputs.execute(context.utxos, operation);
        let mut notes = DepositRecreatedNotes::default();
        for utxo in outputs.utxos(operation) {
            context.channels = context
                .channels
                .register_channel_note(&utxo.id(), &operation.channel_id)?;
            notes
                .try_push(DepositNote {
                    note_id: utxo.id(),
                    value: utxo.note.value,
                    pk: utxo.note.pk,
                })
                .map_err(InputsError::from)?;
        }

        let events = std::iter::once(TxEvent::new(
            context.tx_hash,
            operation.op_id(),
            TxEventPayload::Deposit {
                channel_id: operation.channel_id,
                amount: amount_deposited,
                metadata: operation.metadata.clone(),
                notes,
            },
        ))
        .collect();

        Ok((context, events))
    }
}

impl<State: VerificationState, Mode: VerificationMode> SignedOperationExecutionGas
    for SignedOperation<DepositOp, State, Mode>
{
    fn gas_multiplier(&self) -> Value {
        1
    }
}

#[cfg(test)]
mod test {
    use lb_groth16::CompressedGroth16Proof;
    use lb_key_management_system_keys::keys::ZkKey;
    use num_bigint::BigUint;

    use super::*;
    use crate::mantle::{
        Note, Utxo, ops::op_proof::samples::SampleProof as _,
        transactions::tx_list::signed_ops::test_utils::make_channel_state,
    };

    #[test]
    fn preverify_rejects_empty_inputs() {
        let deposit = DepositOp {
            channel_id: ChannelId::from([0u8; 32]),
            inputs: Inputs::empty(),
            metadata: Metadata::empty(),
        };
        let proof = ZkSignature::new(CompressedGroth16Proof::from_bytes(&[0u8; 128]));
        let signed_operation = SignedOperation::new(deposit, proof);

        assert_eq!(
            signed_operation.preverify(&()),
            Err(Error::Inputs(InputsError::EmptyInputs))
        );
    }

    #[test]
    fn verify_rejects_an_unregistered_channel() {
        let input_key = ZkKey::from(BigUint::from(1u8));
        let input_utxo = Utxo {
            op_id: [1u8; 32],
            output_index: 0,
            note: Note::new(10_000, input_key.to_public_key()),
        };
        let (utxos, _) = Utxos::new().insert(input_utxo.id(), input_utxo);

        let operation = DepositOp {
            inputs: Inputs::new([input_utxo.id()]),
            ..DepositOp::sample()
        };
        let channel_id = operation.channel_id;

        let signed_view = TxHashView::from(TxHash::from([9u8; 32]));
        let proof =
            ZkKey::multi_sign(&[input_key], signed_view.as_fr()).expect("signing should succeed");

        let signed_operation =
            SignedOperation::<_, Unverified, StandardMode>::new(operation, proof)
                .into_preverified(&())
                .expect("preverify should accept a non-empty deposit");

        let locked_notes = LockedNotes::new();

        assert_eq!(
            signed_operation.verify(&DepositValidationContext {
                channels: &Channels::new(),
                locked_notes: &locked_notes,
                utxos: &utxos,
                tx_hash_view: &signed_view,
            }),
            Err(Error::ChannelNotFound { channel_id })
        );
    }

    #[test]
    fn verify_rejects_an_input_missing_from_the_ledger() {
        let input_key = ZkKey::from(BigUint::from(1u8));
        let input_utxo = Utxo {
            op_id: [1u8; 32],
            output_index: 0,
            note: Note::new(10_000, input_key.to_public_key()),
        };

        let operation = DepositOp {
            inputs: Inputs::new([input_utxo.id()]),
            ..DepositOp::sample()
        };

        let mut channels = Channels::new();
        channels
            .channels
            .insert_mut(operation.channel_id, make_channel_state(1, None));
        let locked_notes = LockedNotes::new();

        let signed_view = TxHashView::from(TxHash::from([9u8; 32]));
        let proof =
            ZkKey::multi_sign(&[input_key], signed_view.as_fr()).expect("signing should succeed");

        let signed_operation =
            SignedOperation::<_, Unverified, StandardMode>::new(operation, proof)
                .into_preverified(&())
                .expect("preverify should accept a non-empty deposit");

        assert_eq!(
            signed_operation.verify(&DepositValidationContext {
                channels: &channels,
                locked_notes: &locked_notes,
                utxos: &Utxos::new(),
                tx_hash_view: &signed_view,
            }),
            Err(Error::Inputs(InputsError::InexistingNote(input_utxo.id())))
        );
    }

    #[test]
    fn verify_rejects_an_input_already_owned_by_a_channel() {
        let input_key = ZkKey::from(BigUint::from(1u8));
        let input_utxo = Utxo {
            op_id: [1u8; 32],
            output_index: 0,
            note: Note::new(10_000, input_key.to_public_key()),
        };
        let (utxos, _) = Utxos::new().insert(input_utxo.id(), input_utxo);

        let operation = DepositOp {
            inputs: Inputs::new([input_utxo.id()]),
            ..DepositOp::sample()
        };

        let mut channels = Channels::new();
        channels
            .channels
            .insert_mut(operation.channel_id, make_channel_state(1, None));
        let channels = channels
            .register_channel_note(&input_utxo.id(), &ChannelId::from([21u8; 32]))
            .expect("the note is not owned by another channel");
        let locked_notes = LockedNotes::new();

        let signed_view = TxHashView::from(TxHash::from([9u8; 32]));
        let proof =
            ZkKey::multi_sign(&[input_key], signed_view.as_fr()).expect("signing should succeed");

        let signed_operation =
            SignedOperation::<_, Unverified, StandardMode>::new(operation, proof)
                .into_preverified(&())
                .expect("preverify should accept a non-empty deposit");

        assert_eq!(
            signed_operation.verify(&DepositValidationContext {
                channels: &channels,
                locked_notes: &locked_notes,
                utxos: &utxos,
                tx_hash_view: &signed_view,
            }),
            Err(Error::Inputs(InputsError::ChannelNote(input_utxo.id())))
        );
    }

    #[test]
    fn verify_rejects_a_proof_from_a_key_that_does_not_own_the_input() {
        let input_key = ZkKey::from(BigUint::from(1u8));
        let other_key = ZkKey::from(BigUint::from(7u8));
        let input_utxo = Utxo {
            op_id: [1u8; 32],
            output_index: 0,
            note: Note::new(10_000, input_key.to_public_key()),
        };
        let (utxos, _) = Utxos::new().insert(input_utxo.id(), input_utxo);

        let operation = DepositOp {
            inputs: Inputs::new([input_utxo.id()]),
            ..DepositOp::sample()
        };

        let mut channels = Channels::new();
        channels
            .channels
            .insert_mut(operation.channel_id, make_channel_state(1, None));
        let locked_notes = LockedNotes::new();

        let signed_view = TxHashView::from(TxHash::from([9u8; 32]));
        let proof =
            ZkKey::multi_sign(&[other_key], signed_view.as_fr()).expect("signing should succeed");

        let signed_operation =
            SignedOperation::<_, Unverified, StandardMode>::new(operation, proof)
                .into_preverified(&())
                .expect("preverify should accept a non-empty deposit");

        assert_eq!(
            signed_operation.verify(&DepositValidationContext {
                channels: &channels,
                locked_notes: &locked_notes,
                utxos: &utxos,
                tx_hash_view: &signed_view,
            }),
            Err(Error::InvalidSignature)
        );
    }

    #[test]
    fn verify_rejects_a_proof_over_another_transaction() {
        let input_key = ZkKey::from(BigUint::from(1u8));
        let input_utxo = Utxo {
            op_id: [1u8; 32],
            output_index: 0,
            note: Note::new(10_000, input_key.to_public_key()),
        };
        let (utxos, _) = Utxos::new().insert(input_utxo.id(), input_utxo);

        let operation = DepositOp {
            inputs: Inputs::new([input_utxo.id()]),
            ..DepositOp::sample()
        };

        let mut channels = Channels::new();
        channels
            .channels
            .insert_mut(operation.channel_id, make_channel_state(1, None));
        let locked_notes = LockedNotes::new();

        let signed_view = TxHashView::from(TxHash::from([9u8; 32]));
        let other_view = TxHashView::from(TxHash::from([10u8; 32]));
        let proof =
            ZkKey::multi_sign(&[input_key], signed_view.as_fr()).expect("signing should succeed");

        let signed_operation =
            SignedOperation::<_, Unverified, StandardMode>::new(operation, proof)
                .into_preverified(&())
                .expect("preverify should accept a non-empty deposit");

        assert_eq!(
            signed_operation.verify(&DepositValidationContext {
                channels: &channels,
                locked_notes: &locked_notes,
                utxos: &utxos,
                tx_hash_view: &signed_view,
            }),
            Ok(())
        );
        assert_eq!(
            signed_operation.verify(&DepositValidationContext {
                channels: &channels,
                locked_notes: &locked_notes,
                utxos: &utxos,
                tx_hash_view: &other_view,
            }),
            Err(Error::InvalidSignature)
        );
    }

    #[test]
    fn deposit_op_execution_gas() {
        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
            DepositOp::sample(),
            <DepositOp as ProvableOperation>::Proof::sample(),
        );

        assert_eq!(
            signed_operation.execution_gas::<MainnetGasProfile>(),
            Ok(Gas::new(590))
        );
    }
}
