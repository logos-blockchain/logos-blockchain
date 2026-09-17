use bytes::Bytes;
use lb_binary_codec::bincode::{BoundedSerializeOp, UpperBoundedVec};
use lb_core::header::HeaderId;
use lb_cryptarchia_engine::Slot;
use serde::{Deserialize, Serialize};

const BINCODE_ENUM_DISCRIMINANT_SIZE: usize = size_of::<u32>();

const GET_TIP_BINCODE_SIZE: usize =
    BINCODE_ENUM_DISCRIMINANT_SIZE + size_of::<[u8; 32]>() + size_of::<Slot>() + size_of::<u64>();

/// Maximum configured-bincode size of a tip response. The fixed tip variant is
/// larger than the finite set of typed failure reasons.
pub const MAX_GET_TIP_RESPONSE_BINCODE_SIZE: usize = GET_TIP_BINCODE_SIZE;

/// Blocks are serialized using logos-blockchain-core's wire format.
pub type SerialisedBlock = Bytes;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("Node is not in online mode")]
pub enum GetTipResponseReason {
    NodeNotOnline,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum GetTipResponse {
    /// A success response.
    Tip {
        tip: HeaderId,
        slot: Slot,
        height: u64,
    },
    /// A response indicating that the request failed.
    Failure(GetTipResponseReason),
}

impl BoundedSerializeOp for GetTipResponse {
    type Bytes = UpperBoundedVec<u8, MAX_GET_TIP_RESPONSE_BINCODE_SIZE>;
}

#[cfg(test)]
mod tests {
    use lb_binary_codec::bincode::{DeserializeOp as _, SerializeOp as _};

    use super::*;

    #[test]
    fn tip_response_reasons_round_trip_within_the_bound() {
        let tip = GetTipResponse::Tip {
            tip: HeaderId::from([0; 32]),
            slot: Slot::new(u64::MAX),
            height: u64::MAX,
        };
        let reasons = [(
            GetTipResponseReason::NodeNotOnline,
            2 * BINCODE_ENUM_DISCRIMINANT_SIZE,
        )];

        let tip_bytes = tip.to_bytes().unwrap();
        assert_eq!(tip_bytes.len(), MAX_GET_TIP_RESPONSE_BINCODE_SIZE);
        assert_eq!(tip.to_bounded_bytes().unwrap().as_slice(), tip_bytes);

        for (reason, expected_size) in reasons {
            let response = GetTipResponse::Failure(reason);
            let bytes = response.to_bytes().unwrap();
            assert_eq!(bytes.len(), expected_size);
            assert!(bytes.len() <= MAX_GET_TIP_RESPONSE_BINCODE_SIZE);
            assert_eq!(response.to_bounded_bytes().unwrap().as_slice(), bytes);
            assert!(matches!(
                GetTipResponse::from_bytes(&bytes).unwrap(),
                GetTipResponse::Failure(GetTipResponseReason::NodeNotOnline)
            ));
        }
    }
}
