use lb_key_management_system_keys::keys::{ZkPublicKey, ZkSignature};
use lb_utils::bounded::UpperBoundedVec;
use serde::{Deserialize, Serialize};

use crate::{
    events::{TxEvent, TxEventPayload},
    mantle::{
        channel::{Channels, Error},
        ledger::{Inputs, InputsError, Operation, Outputs, Utxos},
        nom::{NomCodec, NomEncode as _},
        ops::{OpId, channel::ChannelId},
        transactions::hash::TxHash,
    },
    sdp::locked_notes::LockedNotes,
};

pub const MAX_METADATA_SIZE: usize = u32::MAX as usize;
pub type Metadata = UpperBoundedVec<u8, { MAX_METADATA_SIZE }>;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, NomCodec)]
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
        self.encode()
    }
}

pub struct DepositValidationContext<'a> {
    pub channels: &'a Channels,
    pub locked_notes: &'a LockedNotes,
    pub utxos: &'a Utxos,
    pub tx_hash: &'a TxHash,
    pub deposit_sig: &'a ZkSignature,
}

pub struct DepositExecutionContext {
    pub channels: Channels,
    pub utxos: Utxos,
    pub tx_hash: TxHash,
}

impl Operation<DepositValidationContext<'_>> for DepositOp {
    type ExecutionContext<'a>
        = DepositExecutionContext
    where
        Self: 'a;
    type Error = Error;

    fn validate(&self, ctx: &DepositValidationContext<'_>) -> Result<(), Self::Error> {
        // Check that the channel exist
        if !ctx.channels.contains_channel(&self.channel_id) {
            return Err(Error::ChannelNotFound {
                channel_id: self.channel_id,
            });
        }

        // Check that inputs are spendable and not already channel notes
        self.inputs
            .validate_not_in_channel(ctx.locked_notes, ctx.channels, ctx.utxos)?;

        // Check the signature
        let pks = self.inputs.get_pk(ctx.utxos)?;
        if !ZkPublicKey::verify_multi(&pks, &ctx.tx_hash.to_fr(), ctx.deposit_sig) {
            return Err(Error::InvalidSignature);
        }

        Ok(())
    }

    fn execute(
        &self,
        mut ctx: Self::ExecutionContext<'_>,
    ) -> Result<(Self::ExecutionContext<'_>, Vec<TxEvent>), Self::Error> {
        // Get the amount deposited for the event payload
        let amount_deposited = self.inputs.amount(&ctx.utxos)?;
        let outputs = self.outputs(&ctx.utxos)?;

        // Remove the inputs from the ledger.
        ctx.utxos = self.inputs.execute(ctx.utxos)?;

        // Add the re-created notes to the ledger and register them as channel
        // notes.
        ctx.utxos = outputs.execute(ctx.utxos, self);
        let mut note_ids = Vec::with_capacity(outputs.len());
        for utxo in outputs.utxos(self) {
            ctx.channels = ctx
                .channels
                .register_channel_note(&utxo.id(), &self.channel_id)?;
            note_ids.push(utxo.id());
        }

        let events = std::iter::once(TxEvent::new(
            ctx.tx_hash,
            self.op_id(),
            TxEventPayload::Deposit {
                channel_id: self.channel_id,
                amount: amount_deposited,
                metadata: self.metadata.clone(),
                notes: note_ids,
            },
        ))
        .collect();

        Ok((ctx, events))
    }
}
