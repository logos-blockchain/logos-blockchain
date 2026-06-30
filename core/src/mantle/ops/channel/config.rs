use lb_cryptarchia_engine::Slot;
use lb_utils::bounded_vec::NonEmptyBoundedVec;
use serde::{Deserialize, Serialize};

use super::{ChannelId, Ed25519PublicKey, MsgId};
use crate::{
    crypto::{Digest as _, Hasher},
    events::TxEvent,
    mantle::{
        TxHash,
        channel::{ChannelState, Channels, Error, SlotTimeframe, SlotTimeout},
        ledger::Operation,
        nom::{NomCodec, NomEncode as _},
    },
    proofs::channel_multi_sig_proof::ChannelMultiSigProof,
};

pub const CHANNEL_MAX_KEYS: usize = u16::MAX as usize;
pub type Keys = NonEmptyBoundedVec<Ed25519PublicKey, CHANNEL_MAX_KEYS>;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, NomCodec)]
pub struct ChannelConfigOp {
    pub channel: ChannelId,
    pub keys: Keys,
    pub posting_timeframe: SlotTimeframe,
    pub posting_timeout: SlotTimeout,
    pub configuration_threshold: u16,
    pub withdraw_threshold: u16,
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
    pub tx_hash: &'a TxHash,
    pub config_sigs: &'a ChannelMultiSigProof,
}

pub struct ChannelConfigExecutionContext {
    pub channels: Channels,
    pub block_slot: Slot,
}

impl Operation<ChannelConfigValidationContext<'_>> for ChannelConfigOp {
    type ExecutionContext<'a>
        = ChannelConfigExecutionContext
    where
        Self: 'a;
    type Error = Error;

    fn validate(&self, ctx: &ChannelConfigValidationContext<'_>) -> Result<(), Self::Error> {
        // Check that the indexes are unique and there is the same number of proof and
        // index. This is enforced by the proof structure that enforces it.

        // Check config wellformness
        if self.configuration_threshold == 0 || self.withdraw_threshold == 0 || self.keys.is_empty()
        {
            return Err(Error::InvalidChannelConfig);
        }

        if let Some(channel) = ctx.channels.channels.get(&self.channel).cloned() {
            // Check there is enough signatures
            let signatures = ctx.config_sigs.signatures();
            if signatures.len() != channel.configuration_threshold as usize {
                return Err(Error::ThresholdUnmet {
                    channel_id: self.channel,
                    threshold: channel.configuration_threshold,
                    actual: ctx.config_sigs.signatures().len(),
                });
            }

            // Check the signatures
            for sig in signatures {
                if channel
                    .accredited_keys
                    .get(sig.channel_key_index as usize)
                    .ok_or_else(|| Error::InvalidSignatureIndex {
                        channel_id: self.channel,
                        sequencers: channel.accredited_keys.len(),
                        index: sig.channel_key_index,
                    })?
                    .verify(ctx.tx_hash.as_signing_bytes().as_ref(), &sig.signature)
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
    ) -> Result<(Self::ExecutionContext<'_>, Vec<TxEvent>), Self::Error> {
        // if the channel doesn't exist, create it otherwise just update the config
        if let Some(channel) = ctx.channels.channels.get_mut(&self.channel) {
            channel.accredited_keys = self.keys.clone().into();
            channel.configuration_threshold = self.configuration_threshold;
            channel.tip_sequencer = 0;
            channel.tip_sequencer_starting_slot = ctx.block_slot;
            channel.posting_timeframe = self.posting_timeframe.clone();
            channel.posting_timeout = self.posting_timeout.clone();
            channel.withdraw_threshold = self.withdraw_threshold;
            channel.tip_slot = ctx.block_slot;
            channel.tip_message = self.id();
        } else {
            ctx.channels.channels = ctx.channels.channels.insert(
                self.channel,
                ChannelState {
                    accredited_keys: self.keys.clone().into(),
                    configuration_threshold: self.configuration_threshold,
                    tip_message: self.id(),
                    tip_slot: ctx.block_slot,
                    tip_sequencer: 0,
                    tip_sequencer_starting_slot: ctx.block_slot,
                    posting_timeframe: self.posting_timeframe.clone(),
                    balance: 0,
                    withdraw_threshold: self.withdraw_threshold,
                    withdrawal_nonce: 0,
                    posting_timeout: self.posting_timeout.clone(),
                },
            );
        }
        Ok((ctx, Vec::new()))
    }
}
