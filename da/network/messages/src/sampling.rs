use lb_core::da::BlobId;
use lb_kzgrs_backend::common::{ShareIndex, share::DaSharesCommitments};
use serde::{Deserialize, Serialize};

use crate::common::LightShare;

#[repr(C)]
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub enum SampleErrorType {
    NotFound,
}

#[repr(C)]
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShareError {
    pub blob_id: BlobId,
    pub column_idx: ShareIndex,
    pub error_type: SampleErrorType,
    pub error_description: String,
}

#[repr(C)]
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommitmentsError {
    pub blob_id: BlobId,
    pub error_type: SampleErrorType,
    pub error_description: String,
}

#[repr(C)]
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub enum SampleError {
    Share(ShareError),
    Commitments(CommitmentsError),
}

impl SampleError {
    pub fn new_share(
        blob_id: BlobId,
        column_idx: ShareIndex,
        error_type: SampleErrorType,
        error_description: impl Into<String>,
    ) -> Self {
        Self::Share(ShareError {
            blob_id,
            column_idx,
            error_type,
            error_description: error_description.into(),
        })
    }

    pub fn new_commitments(
        blob_id: BlobId,
        error_type: SampleErrorType,
        error_description: impl Into<String>,
    ) -> Self {
        Self::Commitments(CommitmentsError {
            blob_id,
            error_type,
            error_description: error_description.into(),
        })
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub enum SampleRequest {
    Share(SampleShare),
    Commitments(SampleCommitments),
}

#[repr(C)]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct SampleShare {
    pub blob_id: BlobId,
    pub share_idx: ShareIndex,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct SampleCommitments {
    pub blob_id: BlobId,
}

impl SampleRequest {
    #[must_use]
    pub const fn new_share(blob_id: BlobId, share_idx: ShareIndex) -> Self {
        Self::Share(SampleShare { blob_id, share_idx })
    }

    #[must_use]
    pub const fn new_commitments(blob_id: BlobId) -> Self {
        Self::Commitments(SampleCommitments { blob_id })
    }
}

#[repr(C)]
#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SampleResponse {
    Share(LightShare),
    Commitments(DaSharesCommitments),
    Error(SampleError),
}
