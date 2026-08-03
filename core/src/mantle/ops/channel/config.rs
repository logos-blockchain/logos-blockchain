use lb_codec::{BinaryCodec, BinaryEncode as _};
use lb_cryptarchia_engine::Slot;
use lb_utils::bounded::NonEmptyBoundedVec;
use serde::{Deserialize, Serialize};

use super::{ChannelId, Ed25519PublicKey, MsgId};
use crate::{
    crypto::{Digest as _, Hasher},
    events::TxEvent,
    mantle::{
        channel::{ChannelState, Channels, Error, SlotTimeframe, SlotTimeout},
        ledger::Operation,
        transactions::hash::TxHashView,
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
    pub proof: &'a ChannelMultiSigProof,
}

pub struct ChannelConfigExecutionContext {
    pub channels: Channels,
    pub block_slot: Slot,
}

impl Operation<ChannelConfigValidationContext<'_>> for ChannelConfigOp {
    type PreverificationContext<'a>
        = ()
    where
        Self: 'a;
    type ExecutionContext<'a>
        = ChannelConfigExecutionContext
    where
        Self: 'a;
    type VerificationError = Error;
    type ExecutionError = Error;

    fn preverify(
        &self,
        _context: &Self::PreverificationContext<'_>,
    ) -> Result<(), Self::VerificationError> {
        // Check config is well-formed
        if self.configuration_threshold == 0 || self.transfer_threshold == 0 || self.keys.is_empty()
        {
            return Err(Error::InvalidChannelConfig);
        }

        Ok(())
    }

    fn verify(&self, ctx: &ChannelConfigValidationContext<'_>) -> Result<(), Self::ExecutionError> {
        // Check that the indexes are unique and there is the same number of proof and
        // index. This is enforced by the proof structure that enforces it.

        if let Some(channel) = ctx.channels.channel_state(&self.channel) {
            // Check there is enough signatures
            let signatures = ctx.proof.signatures();
            if signatures.len() != channel.configuration_threshold as usize {
                return Err(Error::ThresholdUnmet {
                    channel_id: self.channel,
                    threshold: channel.configuration_threshold,
                    actual: ctx.proof.signatures().len(),
                });
            }

            // Check the signatures
            for signature in signatures {
                if channel
                    .accredited_keys
                    .get(signature.channel_key_index as usize)
                    .ok_or_else(|| Error::InvalidSignatureIndex {
                        channel_id: self.channel,
                        sequencers: channel.accredited_keys.len(),
                        index: signature.channel_key_index,
                    })?
                    .verify(ctx.tx_hash_view.as_bytes(), &signature.signature)
                    .is_err()
                {
                    return Err(Error::InvalidSignature);
                }
            }
        }

        Ok(())
    }

    fn execute(
        &self,
        mut ctx: Self::ExecutionContext<'_>,
    ) -> Result<(Self::ExecutionContext<'_>, Vec<TxEvent>), Self::ExecutionError> {
        let channel = ChannelState {
            accredited_keys: self.keys.clone().into(),
            configuration_threshold: self.configuration_threshold,
            tip_message: self.id(),
            tip_slot: ctx.block_slot,
            tip_sequencer: 0,
            tip_sequencer_starting_slot: ctx.block_slot,
            posting_timeframe: self.posting_timeframe.clone(),
            transfer_threshold: self.transfer_threshold,
            posting_timeout: self.posting_timeout.clone(),
        };

        // if the channel doesn't exist, create it otherwise just update the config
        ctx.channels = ctx.channels.set_channel_state(&self.channel, channel);
        Ok((ctx, Vec::new()))
    }
}
