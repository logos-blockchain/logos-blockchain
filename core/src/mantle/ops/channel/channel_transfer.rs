use lb_codec::{BinaryCodec, BinaryEncode as _};
use serde::{Deserialize, Serialize};

use crate::{
    events::TxEvent,
    mantle::{
        TxHash, Value,
        channel::{Channels, Error},
        gas::{Gas, MainnetGasProfile, OperationGas, SignedOperationExecutionGas},
        ledger::{
            ExecutableOperation, Inputs, Outputs, PreverifiableOperation, ProvableOperation, Utxo,
            Utxos, VerifiableOperation,
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

// ChannelTransfer = ChannelId Inputs Outputs — plain field-order concat.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, BinaryCodec)]
pub struct ChannelTransferOp {
    pub channel_id: ChannelId,
    pub inputs: Inputs,
    pub outputs: Outputs,
}

impl ChannelTransferOp {
    pub fn utxos(&self) -> impl Iterator<Item = Utxo> {
        self.outputs.utxos(self)
    }
}

impl OpId for ChannelTransferOp {
    fn op_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }
}

pub struct ChannelTransferValidationContext<'a> {
    pub channels: &'a Channels,
    pub locked_notes: &'a LockedNotes,
    pub utxos: &'a Utxos,
    pub tx_hash_view: &'a TxHashView,
    pub op_index: usize,
    pub helper: &'a dyn OperationVerificationHelper,
}

pub struct ChannelTransferExecutionContext {
    pub channels: Channels,
    pub utxos: Utxos,
    pub tx_hash: TxHash,
}

impl ProvableOperation for ChannelTransferOp {
    // `SignedOperationExecutionGas::gas_multiplier` below reads this proof's
    // signature count. If this changes, update that too.
    type Proof = ChannelMultiSigProof;
    const CODE: u8 = 0x14;
}

impl OperationGas<MainnetGasProfile> for ChannelTransferOp {
    const GAS_COST: Gas = Gas::new(56);
}

impl PreverifiableOperation<StandardMode>
    for SignedOperation<ChannelTransferOp, Unverified, StandardMode>
{
    type Context<'a> = ();
    type Error = Error;

    fn preverify(&self, _context: &Self::Context<'_>) -> Result<(), Self::Error> {
        let operation = self.operation();

        // Ensure the inputs is non-empty
        operation.inputs.preverify()?;

        // Check that the outputs are valid
        operation.outputs.validate()?;

        Ok(())
    }
}

impl VerifiableOperation<StandardMode>
    for SignedOperation<ChannelTransferOp, Preverified, StandardMode>
{
    type Context<'a> = ChannelTransferValidationContext<'a>;
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

        // Check that the channel exist
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

        // Check the balance is preserved
        let input_amount = operation.inputs.amount(context.utxos)?;
        let output_amount = operation.outputs.amount()?;
        if input_amount != output_amount {
            return Err(Error::UnbalancedTransfer);
        }

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
    for SignedOperation<ChannelTransferOp, Verified, Mode>
{
    type Context<'a> = ChannelTransferExecutionContext;
    type Error = Error;

    fn execute<'a>(
        &self,
        mut context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        let operation = self.operation();

        // Remove the inputs from the ledger and from the channel.
        context.utxos = operation.inputs.execute(context.utxos)?;
        for note_id in operation.inputs.iter() {
            context.channels = context
                .channels
                .unregister_channel_note(note_id, &operation.channel_id)?;
        }

        // Add the outputs to the ledger and register them as channel notes.
        context.utxos = operation.outputs.execute(context.utxos, operation);
        for utxo in operation.utxos() {
            context.channels = context
                .channels
                .register_channel_note(&utxo.id(), &operation.channel_id)?;
        }

        Ok((context, Vec::new()))
    }
}

impl<State: VerificationState, Mode: VerificationMode> SignedOperationExecutionGas
    for SignedOperation<ChannelTransferOp, State, Mode>
{
    fn gas_multiplier(&self) -> Value {
        let signature_count = self.proof().signatures().len();
        Value::try_from(signature_count)
            .expect("Channel multi-signature proofs are bound to u16::MAX signatures.")
    }
}

#[cfg(test)]
mod test {
    use lb_key_management_system_keys::keys::ZkPublicKey;

    use super::*;
    use crate::mantle::{Note, ledger::InputsError};

    #[test]
    fn test_preverify_rejects_empty_inputs() {
        let channel_transfer = ChannelTransferOp {
            channel_id: ChannelId::from([0u8; 32]),
            inputs: Inputs::empty(),
            outputs: Outputs::new([Note::new(100, ZkPublicKey::zero())]),
        };
        let proof = ChannelMultiSigProof::try_new([].into()).unwrap();
        let signed_operation = SignedOperation::new(channel_transfer, proof);

        assert_eq!(
            signed_operation.preverify(&()),
            Err(Error::Inputs(InputsError::EmptyInputs))
        );
    }

    // An empty input list paired with an empty output list is trivially
    // balanced, so the emptiness check is what rejects it.
    #[test]
    fn test_preverify_rejects_empty_inputs_and_outputs() {
        let channel_transfer = ChannelTransferOp {
            channel_id: ChannelId::from([0u8; 32]),
            inputs: Inputs::empty(),
            outputs: Outputs::empty(),
        };
        let proof = ChannelMultiSigProof::try_new([].into()).unwrap();
        let signed_operation = SignedOperation::new(channel_transfer, proof);

        assert_eq!(
            signed_operation.preverify(&()),
            Err(Error::Inputs(InputsError::EmptyInputs))
        );
    }
}
