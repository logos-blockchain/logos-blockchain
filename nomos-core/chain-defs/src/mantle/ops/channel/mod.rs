pub mod blob;
pub mod inscribe;
pub mod set_keys;

use crate::utils::serde_bytes_newtype;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ChannelId([u8; 32]);
serde_bytes_newtype!(ChannelId, 32);

/// The id of the previous message in the channel
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct MsgId([u8; 32]);
serde_bytes_newtype!(MsgId, 32);

pub type Ed25519PublicKey = key_management_system_keys::keys::Ed25519PublicKey;

impl MsgId {
    #[must_use]
    pub const fn root() -> Self {
        Self([0; 32])
    }
}

impl From<[u8; 32]> for MsgId {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl AsRef<[u8; 32]> for MsgId {
    fn as_ref(&self) -> &[u8; 32] {
        &self.0
    }
}

impl From<MsgId> for [u8; 32] {
    fn from(msg_id: MsgId) -> Self {
        msg_id.0
    }
}

impl From<[u8; 32]> for ChannelId {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}
impl AsRef<[u8; 32]> for ChannelId {
    fn as_ref(&self) -> &[u8; 32] {
        &self.0
    }
}

impl From<ChannelId> for [u8; 32] {
    fn from(channel_id: ChannelId) -> Self {
        channel_id.0
    }
}
