use bytes::Bytes;
use lb_core::{
    codec::{BoundedSerializeOp, UpperBoundedVec},
    header::HeaderId,
};
use lb_cryptarchia_engine::Slot;
use serde::{Deserialize, Serialize};

use crate::libp2p::MAX_MSG_LEN;

/// Blocks are serialized using logos-blockchain-core's wire format.
pub type SerialisedBlock = Bytes;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum GetTipResponse {
    /// A success response.
    Tip {
        tip: HeaderId,
        slot: Slot,
        height: u64,
    },
    /// A response indicating that the request failed.
    Failure(String),
}

impl BoundedSerializeOp for GetTipResponse {
    type Bytes = UpperBoundedVec<u8, MAX_MSG_LEN>;
}
