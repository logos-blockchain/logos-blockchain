use lb_codec::{BinaryCodec, BinaryEncode as _};
use lb_cryptarchia_engine::Slot;
use lb_utils::bounded::NonEmptyBoundedVec;
use serde::{Deserialize, Serialize};

use super::{ChannelId, Ed25519PublicKey, MsgId};
use crate::{
    crypto::{Digest as _, Hasher},
    events::TxEvent,
    mantle::{
        Value,
        channel::{ChannelState, Channels, Error, SlotTimeframe, SlotTimeout},
        gas::{Gas, MainnetGasProfile, OperationGas, SignedOperationExecutionGas},
        ledger::{
            ExecutableOperation, PreverifiableOperation, ProvableOperation, VerifiableOperation,
            verification_mode::{StandardMode, VerificationMode},
        },
        ops::SignedOperation,
        transactions::{
            hash::TxHashView,
            states::{Preverified, Unverified, VerificationState, Verified},
        },
    },
    proofs::channel_multi_sig_proof::ChannelMultiSigProof,
};

pub const CHANNEL_MAX_KEYS: usize = u16::MAX as usize;
pub type Keys = NonEmptyBoundedVec<Ed25519PublicKey, CHANNEL_MAX_KEYS>;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct ChannelConfigOp {
    pub channel: ChannelId,
    pub keys: Keys,
    pub posting_timeframe: SlotTimeframe,
    pub posting_timeout: SlotTimeout,
    pub configuration_threshold: u16,
    pub transfer_threshold: u16,
}

impl ChannelConfigOp {
    #[must_use]
    pub fn id(&self) -> MsgId {
        let mut hasher = Hasher::new();
        hasher.update(self.encode());
        MsgId(hasher.finalize().into())
    }
}

pub struct ChannelConfigValidationContext<'a> {
    pub channels: &'a Channels,
    pub tx_hash_view: &'a TxHashView,
}

pub struct ChannelConfigExecutionContext {
    pub channels: Channels,
    pub block_slot: Slot,
}

impl ProvableOperation for ChannelConfigOp {
    // `SignedOperationExecutionGas::gas_multiplier` below reads this proof's
    // signature count. If this changes, update that too.
    type Proof = ChannelMultiSigProof;
    const CODE: u8 = 0x10;
}

impl OperationGas<MainnetGasProfile> for ChannelConfigOp {
    const GAS_COST: Gas = Gas::new(56);
}

impl PreverifiableOperation<StandardMode>
    for SignedOperation<ChannelConfigOp, Unverified, StandardMode>
{
    type Context<'a> = ();
    type Error = Error;

    fn preverify(&self, _context: &Self::Context<'_>) -> Result<(), Self::Error> {
        let operation = self.operation();

        // Check config is well-formed
        if operation.configuration_threshold == 0
            || operation.transfer_threshold == 0
            || operation.keys.is_empty()
        {
            return Err(Error::InvalidChannelConfig);
        }

        Ok(())
    }
}

impl VerifiableOperation<StandardMode>
    for SignedOperation<ChannelConfigOp, Preverified, StandardMode>
{
    type Context<'a> = ChannelConfigValidationContext<'a>;
    type Error = Error;

    fn verify(&self, context: &Self::Context<'_>) -> Result<(), Self::Error> {
        let operation = self.operation();
        let proof = self.proof();

        // Check that the indexes are unique and there is the same number of proof and
        // index. This is enforced by the proof structure that enforces it.

        if let Some(channel) = context.channels.channels.get(&operation.channel).cloned() {
            // Check there is enough signatures
            let signatures = proof.signatures();
            if signatures.len() != channel.configuration_threshold as usize {
                return Err(Error::ThresholdUnmet {
                    channel_id: operation.channel,
                    threshold: channel.configuration_threshold,
                    actual: proof.signatures().len(),
                });
            }

            // Check the signatures
            for signature in signatures {
                if channel
                    .accredited_keys
                    .get(signature.channel_key_index as usize)
                    .ok_or_else(|| Error::InvalidSignatureIndex {
                        channel_id: operation.channel,
                        sequencers: channel.accredited_keys.len(),
                        index: signature.channel_key_index,
                    })?
                    .verify(context.tx_hash_view.as_bytes(), &signature.signature)
                    .is_err()
                {
                    return Err(Error::InvalidSignature);
                }
            }
        }

        Ok(())
    }
}

impl<Mode: VerificationMode> ExecutableOperation
    for SignedOperation<ChannelConfigOp, Verified, Mode>
{
    type Context<'a> = ChannelConfigExecutionContext;
    type Error = Error;

    fn execute<'a>(
        &self,
        mut context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        let operation = self.operation();

        // if the channel doesn't exist, create it otherwise just update the config
        if let Some(channel) = context.channels.channels.get_mut(&operation.channel) {
            channel.accredited_keys = operation.keys.clone().into();
            channel.configuration_threshold = operation.configuration_threshold;
            channel.tip_sequencer = 0;
            channel.tip_sequencer_starting_slot = context.block_slot;
            channel.posting_timeframe = operation.posting_timeframe.clone();
            channel.posting_timeout = operation.posting_timeout.clone();
            channel.transfer_threshold = operation.transfer_threshold;
            channel.tip_slot = context.block_slot;
            channel.tip_message = operation.id();
        } else {
            context.channels.channels = context.channels.channels.insert(
                operation.channel,
                ChannelState {
                    accredited_keys: operation.keys.clone().into(),
                    configuration_threshold: operation.configuration_threshold,
                    tip_message: operation.id(),
                    tip_slot: context.block_slot,
                    tip_sequencer: 0,
                    tip_sequencer_starting_slot: context.block_slot,
                    posting_timeframe: operation.posting_timeframe.clone(),
                    transfer_threshold: operation.transfer_threshold,
                    posting_timeout: operation.posting_timeout.clone(),
                },
            );
        }
        Ok((context, Vec::new()))
    }
}

impl<State: VerificationState, Mode: VerificationMode> SignedOperationExecutionGas
    for SignedOperation<ChannelConfigOp, State, Mode>
{
    fn gas_multiplier(&self) -> Value {
        let signature_count = self.proof().signatures().len();
        Value::try_from(signature_count)
            .expect("Channel multi-signature proofs are bound to u16::MAX signatures.")
    }
}
