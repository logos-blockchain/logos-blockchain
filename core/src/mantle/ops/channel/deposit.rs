use lb_codec::{BinaryCodec, BinaryEncode as _};
use lb_key_management_system_keys::keys::{ZkSignature, public_inputs_from_pks};
use lb_utils::bounded::UpperBoundedVec;
use serde::{Deserialize, Serialize};

use crate::{
    events::{DepositNote, DepositRecreatedNotes, TxEvent, TxEventPayload},
    mantle::{
        Value,
        batch::DeferredZkpVerification,
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
    sdp::service_notes::ServiceNotes,
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
}

impl OpId for DepositOp {
    fn op_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }
}

pub struct DepositValidationContext<'a> {
    pub channels: &'a Channels,
    pub service_notes: &'a ServiceNotes,
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

    fn verify(
        &self,
        context: &Self::Context<'_>,
    ) -> Result<Option<DeferredZkpVerification>, Self::Error> {
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
            context.service_notes,
            context.channels,
            context.utxos,
        )?;

        // Defer the proof verification, so that the caller can batch it.
        let public_keys = operation.inputs.get_pk(context.utxos)?;
        let inputs = public_inputs_from_pks((*context.tx_hash_view.as_fr()).into(), &public_keys)
            .map_err(|_| Error::InvalidSignature)?;
        Ok(Some(DeferredZkpVerification::ZkSig(
            *self.proof().as_proof(),
            inputs,
        )))
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

    use super::*;

    #[test]
    fn test_preverify_rejects_empty_inputs() {
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
}
