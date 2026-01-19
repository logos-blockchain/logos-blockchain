use lb_core::da::BlobId;
use lb_kzgrs_backend::common::share::{DaLightShare, DaShare};
use serde::{Deserialize, Serialize};

use crate::SubnetworkId;

#[repr(C)]
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Share {
    pub blob_id: BlobId,
    pub data: DaShare,
}

impl Share {
    #[must_use]
    pub const fn new(blob_id: BlobId, data: DaShare) -> Self {
        Self { blob_id, data }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct LightShare {
    pub blob_id: BlobId,
    pub data: DaLightShare,
}

impl LightShare {
    #[must_use]
    pub const fn new(blob_id: BlobId, data: DaLightShare) -> Self {
        Self { blob_id, data }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShareRequest {
    pub share: Share,
    pub subnetwork_id: SubnetworkId,
}

#[repr(u8)]
#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum CloseMessageReason {
    GracefulShutdown = 0,
    SubnetChange = 1,
    SubnetSampleFailure = 2,
}

#[repr(C)]
#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CloseMessage {
    pub reason: CloseMessageReason,
}

impl CloseMessage {
    #[must_use]
    pub const fn new(reason: CloseMessageReason) -> Self {
        Self { reason }
    }
}
