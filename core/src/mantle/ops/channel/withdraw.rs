use lb_codec::{BinaryCodec, BinaryEncode as _};
use serde::{Deserialize, Serialize};

use crate::{
    events::TxEvent,
    mantle::{
        TxHash, Value,
        channel::{Channels, Error},
        gas::{Gas, MainnetGasProfile, OperationGas, SignedOperationExecutionGas},
        ledger::{
            ExecutableOperation, Inputs, PreverifiableOperation, ProvableOperation, Utxos,
            VerifiableOperation, verification_mode, verification_mode::VerificationMode,
        },
        ops::{
            OpId, SignedOp,
            channel::{ChannelId, verification::verify_channel_multi_sig},
        },
        transactions::{OperationVerificationHelper, hash::TxHashView, states::VerificationState},
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

pub struct WithdrawExecutionContext {
    pub channels: Channels,
    pub tx_hash: TxHash,
}

impl ProvableOperation for ChannelWithdrawOp {
    // `SignedOperationExecutionGas::gas_multiplier` below reads this proof's
    // signature count. If this changes, update that too.
    type Proof = ChannelMultiSigProof;
}

impl OperationGas<MainnetGasProfile> for ChannelWithdrawOp {
    const GAS_COST: Gas = Gas::new(56);
}

impl PreverifiableOperation<verification_mode::StandardMode> for ChannelWithdrawOp {
    type Context<'a> = ();
    type Error = Error;

    fn preverify(
        &self,
        _proof: &Self::Proof,
        _context: &Self::Context<'_>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl VerifiableOperation<verification_mode::StandardMode> for ChannelWithdrawOp {
    type Context<'a> = WithdrawValidationContext<'a>;
    type Error = Error;

    fn verify(&self, proof: &Self::Proof, context: &Self::Context<'_>) -> Result<(), Self::Error> {
        verify_channel_multi_sig(
            &self.channel_id,
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
                .get(&self.channel_id)
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

impl ExecutableOperation for ChannelWithdrawOp {
    type Context<'a> = WithdrawExecutionContext;
    type Error = Error;

    fn execute<'a>(
        &self,
        mut context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        // Release the inputs from the channel. The notes keep their NoteId,
        // value and ZkPublicKey and stay in the ledger as regular notes.
        for note_id in self.inputs.iter() {
            context.channels = context
                .channels
                .unregister_channel_note(note_id, &self.channel_id)?;
        }

        Ok((context, Vec::new()))
    }
}

impl<State: VerificationState, Mode: VerificationMode> SignedOperationExecutionGas
    for SignedOp<ChannelWithdrawOp, State, Mode>
{
    fn gas_multiplier(&self) -> Value {
        let signature_count = self.proof().signatures().len();
        Value::try_from(signature_count)
            .expect("Channel multi-signature proofs are bound to u16::MAX signatures.")
    }
}
