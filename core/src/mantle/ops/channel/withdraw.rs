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
    use super::*;
    use crate::mantle::ledger::InputsError;

    #[test]
    fn test_preverify_rejects_empty_inputs() {
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
