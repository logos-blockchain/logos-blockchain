use lb_codec::{BinaryCodec, BinaryEncode as _};
use serde::{Deserialize, Serialize};

use crate::{
    events::TxEvent,
    mantle::{
        TxHash,
        channel::{Channels, Error},
        ledger::{Inputs, Operation, Outputs, Utxo, Utxos},
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
    pub proof: &'a ChannelMultiSigProof,
    pub op_index: usize,
    pub helper: &'a dyn OperationVerificationHelper,
}

pub struct ChannelTransferExecutionContext {
    pub channels: Channels,
    pub utxos: Utxos,
    pub tx_hash: TxHash,
}

impl Operation<ChannelTransferValidationContext<'_>> for ChannelTransferOp {
    type PreverificationContext<'a>
        = ()
    where
        Self: 'a;
    type ExecutionContext<'a>
        = ChannelTransferExecutionContext
    where
        Self: 'a;
    type VerificationError = Error;
    type ExecutionError = Error;

    fn preverify(
        &self,
        _context: &Self::PreverificationContext<'_>,
    ) -> Result<(), Self::VerificationError> {
        // Check that the outputs are valid
        self.outputs.validate()?;

        Ok(())
    }

    fn verify(
        &self,
        ctx: &ChannelTransferValidationContext<'_>,
    ) -> Result<(), Self::ExecutionError> {
        verify_channel_multi_sig(
            &self.channel_id,
            ctx.proof,
            ctx.tx_hash_view.as_bytes(),
            ctx.helper,
            ctx.op_index,
        )
        .map_err(|_error| Error::InvalidSignature)?; // FIXME: Discards error details

        // Check that the channel exist
        let channel =
            ctx.channels
                .channel_state(&self.channel_id)
                .ok_or(Error::ChannelNotFound {
                    channel_id: self.channel_id,
                })?;

        // Check that the inputs are valid and belong to the channel
        self.inputs.validate_in_channel(
            ctx.locked_notes,
            ctx.channels,
            &self.channel_id,
            ctx.utxos,
        )?;

        // Check the balance is preserved
        let input_amount = self.inputs.amount(ctx.utxos)?;
        let output_amount = self.outputs.amount()?;
        if input_amount != output_amount {
            return Err(Error::UnbalancedTransfer);
        }

        // Check there is enough signatures
        let signatures = ctx.proof.signatures();
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
                .verify(ctx.tx_hash_view.as_bytes(), &sig.signature)
                .is_err()
            {
                return Err(Error::InvalidSignature);
            }
        }

        Ok(())
    }

    fn execute(
        &self,
        mut ctx: Self::ExecutionContext<'_>,
    ) -> Result<(Self::ExecutionContext<'_>, Vec<TxEvent>), Self::ExecutionError> {
        // Remove the inputs from the ledger and from the channel.
        ctx.utxos = self.inputs.execute(ctx.utxos)?;
        for note_id in self.inputs.iter() {
            ctx.channels = ctx
                .channels
                .unregister_channel_note(note_id, &self.channel_id)?;
        }

        // Add the outputs to the ledger and register them as channel notes.
        ctx.utxos = self.outputs.execute(ctx.utxos, self);
        for utxo in self.utxos() {
            ctx.channels = ctx
                .channels
                .register_channel_note(&utxo.id(), &self.channel_id)?;
        }

        Ok((ctx, Vec::new()))
    }
}
