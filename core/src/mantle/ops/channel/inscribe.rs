use std::sync::Arc;

use bytes::Bytes;
use lb_key_management_system_keys::keys::Ed25519Signature;
use serde::{Deserialize, Serialize};

use super::{ChannelId, Ed25519PublicKey, MsgId};
use crate::{
    crypto::{Digest as _, Hasher},
    mantle::{
        TxHash,
        channel::{ChannelState, Channels, Error},
        encoding::encode_channel_inscribe,
        ledger::Operation,
    },
};

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct InscriptionOp {
    pub channel_id: ChannelId,
    /// Message to be written in the blockchain
    pub inscription: Vec<u8>,
    /// Enforce that this inscription comes after this tx
    pub parent: MsgId,
    pub signer: Ed25519PublicKey,
}

impl InscriptionOp {
    #[must_use]
    pub fn id(&self) -> MsgId {
        let mut hasher = Hasher::new();
        hasher.update(self.payload_bytes());
        MsgId(hasher.finalize().into())
    }

    #[must_use]
    fn payload_bytes(&self) -> Bytes {
        encode_channel_inscribe(self).into()
    }
}

pub struct InscriptionValidationContext<'a> {
    pub channels: &'a Channels,
    pub tx_hash: &'a TxHash,
    pub inscribe_sig: &'a Ed25519Signature,
}

impl Operation for InscriptionOp {
    type ValidationContext<'a>
        = InscriptionValidationContext<'a>
    where
        Self: 'a;
    type ExecutionContext<'a>
        = Channels
    where
        Self: 'a;
    type Error = Error;

    fn validate(&self, ctx: &Self::ValidationContext<'_>) -> Result<(), Self::Error> {
        // Check if the channel exist otherwise the inscription is valid only if and
        // only if parent == ZERO
        if let Some(channel) = ctx.channels.channels.get(&self.channel_id).cloned() {
            // Check the parent corresponds to the payload
            if self.parent != channel.tip {
                return Err(Error::InvalidParent {
                    channel_id: self.channel_id,
                    parent: self.parent.into(),
                    actual: channel.tip.into(),
                });
            }

            // Check that the signer is in the list
            if !channel.keys.contains(&self.signer) {
                return Err(Error::UnauthorizedSigner {
                    channel_id: self.channel_id,
                    signer: format!("{signer:?}", signer = self.signer),
                });
            }

            // Check the signature
            if self
                .signer
                .verify(ctx.tx_hash.as_signing_bytes().as_ref(), ctx.inscribe_sig)
                .is_err()
            {
                return Err(Error::InvalidSignature);
            }
        } else if self.parent != MsgId::root() {
            // Checked that the parent is ZERO because channel doesn't exist
            return Err(Error::InvalidParent {
                channel_id: self.channel_id,
                parent: self.parent.into(),
                actual: MsgId::root().into(),
            });
        }

        Ok(())
    }

    fn execute(
        &self,
        mut channels: Self::ExecutionContext<'_>,
    ) -> Result<Self::ExecutionContext<'_>, Self::Error> {
        // if the channel doesn't exist, create it
        let channel = channels
            .channels
            .get(&self.channel_id)
            .cloned()
            .unwrap_or_else(|| ChannelState {
                tip: MsgId::root(),
                keys: vec![self.signer].into(),
                balance: 0,
                withdraw_threshold: crate::mantle::channel::DEFAULT_WITHDRAW_THRESHOLD,
                withdrawal_nonce: 0,
            });

        // Update the channel tip
        channels.channels = channels.channels.insert(
            self.channel_id,
            ChannelState {
                tip: self.id(),
                keys: Arc::clone(&channel.keys),
                balance: channel.balance,
                withdraw_threshold: channel.withdraw_threshold,
                withdrawal_nonce: channel.withdrawal_nonce,
            },
        );
        Ok(channels)
    }
}
