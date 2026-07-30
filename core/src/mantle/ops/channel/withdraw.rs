use lb_codec::{BinaryCodec, BinaryEncode as _};
use serde::{Deserialize, Serialize};

use crate::{
    events::TxEvent,
    mantle::{
        channel::{Channels, Error},
        ledger::{Inputs, Operation, Utxos},
        ops::{OpId, channel::ChannelId},
        transactions::TxHash,
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
    pub tx_hash: &'a TxHash,
    pub withdraw_sigs: &'a ChannelMultiSigProof,
}

pub struct WithdrawExecutionContext {
    pub channels: Channels,
    pub tx_hash: TxHash,
}

impl Operation<WithdrawValidationContext<'_>> for ChannelWithdrawOp {
    type ExecutionContext<'a>
        = WithdrawExecutionContext
    where
        Self: 'a;
    type Error = Error;

    fn validate(&self, ctx: &WithdrawValidationContext<'_>) -> Result<(), Self::Error> {
        // Check that the channel exists
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

        // Check there is enough signatures
        let signatures = ctx.withdraw_sigs.signatures();
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
                .verify(ctx.tx_hash.as_signing_bytes().as_ref(), &sig.signature)
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
    ) -> Result<(Self::ExecutionContext<'_>, Vec<TxEvent>), Self::Error> {
        // Release the inputs from the channel. The notes keep their NoteId,
        // value and ZkPublicKey and stay in the ledger as regular notes.
        for note_id in self.inputs.iter() {
            ctx.channels = ctx
                .channels
                .unregister_channel_note(note_id, &self.channel_id)?;
        }

        Ok((ctx, Vec::new()))
    }
}
