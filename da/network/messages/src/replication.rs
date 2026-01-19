use blake2::Digest as _;
use lb_core::{
    da::BlobId,
    mantle::{SignedMantleTx, Transaction as _},
};
use serde::{Deserialize, Serialize};

use crate::{
    SubnetworkId,
    common::{Share, ShareRequest},
};

#[repr(C)]
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub enum ReplicationRequest {
    Share(ShareRequest),
    Tx(SignedMantleTx),
}

impl ReplicationRequest {
    #[must_use]
    pub fn id(&self) -> ReplicationResponseId {
        match self {
            Self::Share(share) => (share.share.blob_id, share.subnetwork_id).into(),
            Self::Tx(tx) => tx.into(),
        }
    }
}

impl From<SignedMantleTx> for ReplicationRequest {
    fn from(tx: SignedMantleTx) -> Self {
        Self::Tx(tx)
    }
}

impl From<Share> for ReplicationRequest {
    fn from(share: Share) -> Self {
        Self::Share(ShareRequest {
            subnetwork_id: share.data.share_idx,
            share,
        })
    }
}

#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
pub struct ReplicationResponseId([u8; 34]);

impl From<(BlobId, SubnetworkId)> for ReplicationResponseId {
    fn from((blob_id, subnetwork_id): (BlobId, SubnetworkId)) -> Self {
        let mut id = [0; 34];
        id[..32].copy_from_slice(&blob_id);
        id[32..].copy_from_slice(&subnetwork_id.to_le_bytes());
        Self(id)
    }
}

impl From<&SignedMantleTx> for ReplicationResponseId {
    fn from(tx: &SignedMantleTx) -> Self {
        let bytes = tx.hash().as_signing_bytes();
        let mut hasher = blake2::Blake2b::default();
        hasher.update(&bytes);
        Self(hasher.finalize().into())
    }
}
