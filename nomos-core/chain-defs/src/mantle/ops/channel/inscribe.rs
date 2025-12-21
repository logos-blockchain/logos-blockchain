use bytes::Bytes;
use serde::{Deserialize, Serialize};

use super::{ChannelId, Ed25519PublicKey, MsgId};
use crate::{
    crypto::{Digest as _, Hasher},
    mantle::encoding::encode_channel_inscribe,
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
