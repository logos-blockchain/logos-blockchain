use std::sync::Arc;

use key_management_system_keys::keys::Ed25519Signature;
use nomos_core::mantle::{
    TxHash,
    ops::channel::{
        ChannelId, Ed25519PublicKey as PublicKey, MsgId, inscribe::InscriptionOp,
        set_keys::SetKeysOp,
    },
};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum Error {
    #[error("Invalid parent {parent:?} for channel {channel_id:?}, expected {actual:?}")]
    InvalidParent {
        channel_id: ChannelId,
        parent: [u8; 32],
        actual: [u8; 32],
    },
    #[error("Unauthorized signer {signer:?} for channel {channel_id:?}")]
    UnauthorizedSigner {
        channel_id: ChannelId,
        signer: String,
    },
    #[error("Invalid signature")]
    InvalidSignature,
    #[error("Invalid keys for channel {channel_id:?}")]
    EmptyKeys { channel_id: ChannelId },
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channels {
    pub channels: rpds::HashTrieMapSync<ChannelId, ChannelState>,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelState {
    pub tip: MsgId,
    // avoid cloning the keys every new message
    pub keys: Arc<[PublicKey]>,
}

impl Default for Channels {
    fn default() -> Self {
        Self::new()
    }
}

impl Channels {
    pub fn from_genesis(op: &InscriptionOp) -> Result<Self, Error> {
        Self::default().apply_msg(op.channel_id, &op.parent, op.id(), &op.signer)
    }

    pub fn apply_msg(
        mut self,
        channel_id: ChannelId,
        parent: &MsgId,
        msg: MsgId,
        signer: &PublicKey,
    ) -> Result<Self, Error> {
        let channel = self
            .channels
            .get(&channel_id)
            .cloned()
            .unwrap_or_else(|| ChannelState {
                tip: MsgId::root(),
                keys: vec![*signer].into(),
            });

        if *parent != channel.tip {
            return Err(Error::InvalidParent {
                channel_id,
                parent: (*parent).into(),
                actual: channel.tip.into(),
            });
        }

        if !channel.keys.contains(signer) {
            return Err(Error::UnauthorizedSigner {
                channel_id,
                signer: format!("{signer:?}"),
            });
        }

        self.channels = self.channels.insert(
            channel_id,
            ChannelState {
                tip: msg,
                keys: Arc::clone(&channel.keys),
            },
        );
        Ok(self)
    }

    pub fn set_keys(
        mut self,
        channel_id: ChannelId,
        op: &SetKeysOp,
        sig: &Ed25519Signature,
        tx_hash: &TxHash,
    ) -> Result<Self, Error> {
        if op.keys.is_empty() {
            return Err(Error::EmptyKeys { channel_id });
        }

        if let Some(channel) = self.channels.get_mut(&channel_id) {
            if channel.keys[0]
                .verify(tx_hash.as_signing_bytes().as_ref(), sig)
                .is_err()
            {
                return Err(Error::InvalidSignature);
            }
            channel.keys = op.keys.clone().into();
        } else {
            self.channels = self.channels.insert(
                channel_id,
                ChannelState {
                    tip: MsgId::root(),
                    keys: op.keys.clone().into(),
                },
            );
        }

        Ok(self)
    }

    #[must_use]
    pub fn new() -> Self {
        Self {
            channels: rpds::HashTrieMapSync::new_sync(),
        }
    }

    #[must_use]
    pub fn channel_state(&self, channel_id: &ChannelId) -> Option<&ChannelState> {
        self.channels.get(channel_id)
    }
}
