use lb_codec::{BinaryCodec, BinaryEncode as _};
use lb_key_management_system_keys::keys::{ZkPublicKey, ZkSignature};
use lb_utils::bounded::UpperBoundedVec;
use serde::{Deserialize, Serialize};

use crate::{
    events::{DepositRecreatedNotes, TxEvent, TxEventPayload},
    mantle::{
        channel::{Channels, Error},
        ledger::{
            ExecutableOperation, Inputs, InputsError, Outputs, ProvableOperation, Utxos,
            VerifiableOperation, verification_mode,
        },
        ops::{OpId, channel::ChannelId},
        transactions::hash::{TxHash, TxHashView},
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
}

impl VerifiableOperation<verification_mode::StandardMode> for DepositOp {
    type PreverificationContext<'a> = ();
    type VerificationContext<'a> = DepositValidationContext<'a>;
    type Error = Error;

    fn preverify(
        &self,
        _proof: &Self::Proof,
        _context: &Self::PreverificationContext<'_>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn verify(
        &self,
        proof: &Self::Proof,
        context: &Self::VerificationContext<'_>,
    ) -> Result<(), Self::Error> {
        // Check that the channel exist
        if !context.channels.channels.contains_key(&self.channel_id) {
            return Err(Error::ChannelNotFound {
                channel_id: self.channel_id,
            });
        }

        // Check that inputs are spendable and not already channel notes
        self.inputs.validate_not_in_channel(
            context.locked_notes,
            context.channels,
            context.utxos,
        )?;

        // Check the signature
        let public_keys = self.inputs.get_pk(context.utxos)?;
        if !ZkPublicKey::verify_multi(&public_keys, context.tx_hash_view.as_fr(), proof) {
            return Err(Error::InvalidSignature);
        }

        Ok(())
    }
}

impl ExecutableOperation for DepositOp {
    type Context<'a> = DepositExecutionContext;
    type Error = Error;

    fn execute<'a>(
        &self,
        mut context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        // Get the amount deposited for the event payload
        let amount_deposited = self.inputs.amount(&context.utxos)?;
        let outputs = self.outputs(&context.utxos)?;

        // Remove the inputs from the ledger.
        context.utxos = self.inputs.execute(context.utxos)?;

        // Add the re-created notes to the ledger and register them as channel
        // notes.
        context.utxos = outputs.execute(context.utxos, self);
        let mut note_ids = DepositRecreatedNotes::default();
        for utxo in outputs.utxos(self) {
            context.channels = context
                .channels
                .register_channel_note(&utxo.id(), &self.channel_id)?;
            note_ids.try_push(utxo.id()).map_err(InputsError::from)?;
        }

        let events = std::iter::once(TxEvent::new(
            context.tx_hash,
            self.op_id(),
            TxEventPayload::Deposit {
                channel_id: self.channel_id,
                amount: amount_deposited,
                metadata: self.metadata.clone(),
                notes: note_ids,
            },
        ))
        .collect();

        Ok((context, events))
    }
}
