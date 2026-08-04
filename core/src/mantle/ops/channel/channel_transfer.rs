use lb_codec::{BinaryCodec, BinaryEncode as _};
use serde::{Deserialize, Serialize};

use crate::{
    events::TxEvent,
    mantle::{
        TxHash,
        channel::{Channels, Error},
        ledger::{
            ExecutableOperation, Inputs, Outputs, ProvableOperation, Utxo, Utxos,
            VerifiableOperation, verification_mode,
        },
        ops::{
            OpId,
            channel::{ChannelId, verification::verify_channel_multi_sig},
        },
        transactions::{OperationVerificationHelper, hash::TxHashView},
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
    type Proof = ChannelMultiSigProof;
}

impl VerifiableOperation<verification_mode::StandardMode> for ChannelTransferOp {
    type PreverificationContext<'a> = ();
    type VerificationContext<'a> = ChannelTransferValidationContext<'a>;
    type Error = Error;

    fn preverify(
        &self,
        _proof: &Self::Proof,
        _context: &Self::PreverificationContext<'_>,
    ) -> Result<(), Self::Error> {
        // Check that the outputs are valid
        self.outputs.validate()?;

        Ok(())
    }

    fn verify(
        &self,
        proof: &Self::Proof,
        context: &Self::VerificationContext<'_>,
    ) -> Result<(), Self::Error> {
        verify_channel_multi_sig(
            &self.channel_id,
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
                .channel_state(&self.channel_id)
                .ok_or(Error::ChannelNotFound {
                    channel_id: self.channel_id,
                })?;

        // Check that the inputs are valid and belong to the channel
        self.inputs.validate_in_channel(
            context.locked_notes,
            context.channels,
            &self.channel_id,
            context.utxos,
        )?;

        // Check the balance is preserved
        let input_amount = self.inputs.amount(context.utxos)?;
        let output_amount = self.outputs.amount()?;
        if input_amount != output_amount {
            return Err(Error::UnbalancedTransfer);
        }

        // Check there is enough signatures
        let signatures = proof.signatures();
        if signatures.len() != channel.transfer_threshold as usize {
            return Err(Error::ThresholdUnmet {
                channel_id: self.channel_id,
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

impl ExecutableOperation for ChannelTransferOp {
    type Context<'a> = ChannelTransferExecutionContext;
    type Error = Error;

    fn execute<'a>(
        &self,
        mut context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        // Remove the inputs from the ledger and from the channel.
        context.utxos = self.inputs.execute(context.utxos)?;
        for note_id in self.inputs.iter() {
            context.channels = context
                .channels
                .unregister_channel_note(note_id, &self.channel_id)?;
        }

        // Add the outputs to the ledger and register them as channel notes.
        context.utxos = self.outputs.execute(context.utxos, self);
        for utxo in self.utxos() {
            context.channels = context
                .channels
                .register_channel_note(&utxo.id(), &self.channel_id)?;
        }

        Ok((context, Vec::new()))
    }
}
