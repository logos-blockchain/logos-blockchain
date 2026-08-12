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
        batch::DeferredZkpVerification,
        channel::{ChannelState, Channels, Error, SlotTimeframe, SlotTimeout},
        gas::{Gas, MainnetGasProfile, OperationGas, SignedOperationExecutionGas},
        ledger::{
            ExecutableOperation, PreverifiableOperation, ProvableOperation, VerifiableOperation,
            verification_mode::{self, VerificationMode},
        },
        ops::SignedOperation,
        transactions::{hash::TxHashView, states::VerificationState},
    },
    proofs::channel_multi_sig_proof::ChannelMultiSigProof,
};

pub const CHANNEL_MAX_KEYS: usize = u16::MAX as usize;
pub type Keys = NonEmptyBoundedVec<Ed25519PublicKey, CHANNEL_MAX_KEYS>;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct ChannelConfigOp {
    pub channel: ChannelId,
    pub parent: MsgId,
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

impl PreverifiableOperation<verification_mode::StandardMode> for ChannelConfigOp {
    type Context<'a> = ();
    type Error = Error;

    fn preverify(
        &self,
        _proof: &Self::Proof,
        _context: &Self::Context<'_>,
    ) -> Result<(), Self::Error> {
        // Check config is well-formed
        if self.configuration_threshold == 0 || self.transfer_threshold == 0 || self.keys.is_empty()
        {
            return Err(Error::InvalidChannelConfig);
        }

        Ok(())
    }
}

impl VerifiableOperation<verification_mode::StandardMode> for ChannelConfigOp {
    type Context<'a> = ChannelConfigValidationContext<'a>;
    type Error = Error;

    fn verify(
        &self,
        proof: &Self::Proof,
        context: &Self::Context<'_>,
    ) -> Result<Option<DeferredZkpVerification>, Self::Error> {
        // Check that the indexes are unique and there is the same number of proof and
        // index. This is enforced by the proof structure that enforces it.

        if let Some(channel) = context.channels.channels.get(&self.channel).cloned() {
            // Check the configuration extends the last configuration of the channel
            if self.parent != channel.config_tip_hash {
                return Err(Error::InvalidParent {
                    channel_id: self.channel,
                    parent: self.parent.into(),
                    actual: channel.config_tip_hash.into(),
                });
            }

            // Check there is enough signatures
            let signatures = proof.signatures();
            if signatures.len() != channel.configuration_threshold as usize {
                return Err(Error::ThresholdUnmet {
                    channel_id: self.channel,
                    threshold: channel.configuration_threshold,
                    actual: proof.signatures().len(),
                });
            }

            // Check the signatures. Don't defer this because ed25519 verification is cheap.
            for signature in signatures {
                if channel
                    .accredited_keys
                    .get(signature.channel_key_index as usize)
                    .ok_or_else(|| Error::InvalidSignatureIndex {
                        channel_id: self.channel,
                        sequencers: channel.accredited_keys.len(),
                        index: signature.channel_key_index,
                    })?
                    .verify(context.tx_hash_view.as_bytes(), &signature.signature)
                    .is_err()
                {
                    return Err(Error::InvalidSignature);
                }
            }
        } else if self.parent != MsgId::root() {
            // Checked that the parent is ZERO because channel doesn't exist
            return Err(Error::InvalidParent {
                channel_id: self.channel,
                parent: self.parent.into(),
                actual: MsgId::root().into(),
            });
        }

        Ok(None)
    }
}

impl ExecutableOperation for ChannelConfigOp {
    type Context<'a> = ChannelConfigExecutionContext;
    type Error = Error;

    fn execute<'a>(
        &self,
        mut context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        // if the channel doesn't exist, create it otherwise just update the config
        if let Some(channel) = context.channels.channels.get_mut(&self.channel) {
            channel.accredited_keys = self.keys.clone().into();
            channel.configuration_threshold = self.configuration_threshold;
            channel.tip_sequencer = 0;
            channel.tip_sequencer_starting_slot = context.block_slot;
            channel.posting_timeframe = self.posting_timeframe.clone();
            channel.posting_timeout = self.posting_timeout.clone();
            channel.transfer_threshold = self.transfer_threshold;
            channel.tip_slot = context.block_slot;
            channel.config_tip_hash = self.id();
        } else {
            context.channels.channels = context.channels.channels.insert(
                self.channel,
                ChannelState {
                    accredited_keys: self.keys.clone().into(),
                    configuration_threshold: self.configuration_threshold,
                    tip_message: MsgId::root(),
                    config_tip_hash: self.id(),
                    tip_slot: context.block_slot,
                    tip_sequencer: 0,
                    tip_sequencer_starting_slot: context.block_slot,
                    posting_timeframe: self.posting_timeframe.clone(),
                    transfer_threshold: self.transfer_threshold,
                    posting_timeout: self.posting_timeout.clone(),
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
