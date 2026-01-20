use bytes::Bytes;
use serde::{Deserialize, Serialize};

pub(crate) const DA_COLUMNS: u64 = 1024;
pub(crate) const DA_ELEMENT_SIZE: u64 = 32;

use super::{ChannelId, Ed25519PublicKey, MsgId};
use crate::{
    crypto::Digest as _,
    da::BlobId,
    mantle::{encoding::encode_channel_blob, gas::Gas},
    sdp::SessionNumber,
};

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct BlobOp {
    pub channel: ChannelId,
    pub session: SessionNumber,
    pub blob: BlobId,
    pub blob_size: u64,
    pub da_storage_gas_price: Gas,
    pub parent: MsgId,
    pub signer: Ed25519PublicKey,
}

impl BlobOp {
    #[must_use]
    pub fn id(&self) -> MsgId {
        let mut hasher = crate::crypto::Hasher::new();
        hasher.update(self.payload_bytes());
        MsgId(hasher.finalize().into())
    }

    #[must_use]
    fn payload_bytes(&self) -> Bytes {
        encode_channel_blob(self).into()
    }
}
