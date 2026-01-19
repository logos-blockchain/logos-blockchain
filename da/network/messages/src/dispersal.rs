use lb_core::{
    da::{BlobId, blob},
    mantle::{SignedMantleTx, ops::Op},
};
use lb_kzgrs_backend::common::share::DaShare;
use serde::{Deserialize, Serialize};

use crate::common::{Share, ShareRequest};

#[repr(C)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DispersalErrorType {
    ChunkSize,
    BlobVerification,
    TxVerification,
}

#[repr(C)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DispersalError {
    pub blob_id: BlobId,
    pub error_type: DispersalErrorType,
    pub error_description: String,
}

impl DispersalError {
    pub fn new(
        blob_id: BlobId,
        error_type: DispersalErrorType,
        error_description: impl Into<String>,
    ) -> Self {
        Self {
            blob_id,
            error_type,
            error_description: error_description.into(),
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub enum DispersalRequest {
    Share(ShareRequest),
    Tx(SignedMantleTx),
}

impl DispersalRequest {
    #[must_use]
    pub fn blob_id(&self) -> Option<BlobId> {
        match self {
            Self::Share(share) => Some(share.share.blob_id),
            Self::Tx(tx) => match tx.mantle_tx.ops.first() {
                Some(Op::ChannelBlob(blob_op)) => Some(blob_op.blob),
                _ => None,
            },
        }
    }
}

impl From<DaShare> for DispersalRequest {
    fn from(share: DaShare) -> Self {
        Self::Share(ShareRequest {
            subnetwork_id: share.share_idx,
            share: Share {
                blob_id: blob::Share::blob_id(&share),
                data: share,
            },
        })
    }
}

impl From<SignedMantleTx> for DispersalRequest {
    fn from(tx: SignedMantleTx) -> Self {
        Self::Tx(tx)
    }
}

#[repr(C)]
#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DispersalResponse {
    Tx(BlobId),
    BlobId(BlobId),
    Error(DispersalError),
}
