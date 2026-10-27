use bytes::Bytes;
use lb_binary_codec::bincode::{self, BoundedSerializeOp, UpperBoundedVec};
use lb_core::header::HeaderId;
use lb_cryptarchia_engine::Slot;
use serde::{Deserialize, Serialize};

const TIP_BINCODE_SIZE: usize = bincode::BINCODE_ENUM_DISCRIMINANT_SIZE
    + <HeaderId as BoundedSerializeOp>::MAX_ENCODED_SIZE
    + <Slot as BoundedSerializeOp>::MAX_ENCODED_SIZE
    + bincode::BINCODE_U64_SIZE;

/// Maximum configured-bincode size of a tip response. The fixed tip variant is
/// larger than the finite set of typed failure reasons.
pub const MAX_TIP_RESPONSE_BINCODE_SIZE: usize = TIP_BINCODE_SIZE;

/// Blocks are serialized using logos-blockchain-core's wire format.
pub type SerialisedBlock = Bytes;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum TipResponseFailureReason {
    #[error("node is not in online mode")]
    NodeNotOnline,
    #[error("unknown error")]
    Unknown,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum TipResponse {
    /// A success response.
    Tip {
        tip: HeaderId,
        slot: Slot,
        height: u64,
    },
    /// A response indicating that the request failed.
    Failure(TipResponseFailureReason),
}

impl BoundedSerializeOp for TipResponse {
    type Bytes = UpperBoundedVec<u8, MAX_TIP_RESPONSE_BINCODE_SIZE>;
}

#[cfg(test)]
mod tests {
    use lb_binary_codec::bincode::{DeserializeOp as _, SerializeOp as _};

    use super::*;

    #[test]
    fn tip_response_reasons_round_trip_within_the_bound() {
        let tip = TipResponse::Tip {
            tip: HeaderId::from([0; 32]),
            slot: Slot::new(u64::MAX),
            height: u64::MAX,
        };
        let reasons = [
            (
                TipResponseFailureReason::NodeNotOnline,
                2 * bincode::BINCODE_ENUM_DISCRIMINANT_SIZE,
            ),
            (
                TipResponseFailureReason::Unknown,
                2 * bincode::BINCODE_ENUM_DISCRIMINANT_SIZE,
            ),
        ];

        let tip_bytes = tip.to_bytes().unwrap();
        assert_eq!(tip_bytes.len(), MAX_TIP_RESPONSE_BINCODE_SIZE);
        assert_eq!(tip.to_bounded_bytes().unwrap().as_slice(), tip_bytes);

        for (reason, expected_size) in reasons {
            let response = TipResponse::Failure(reason);
            let bytes = response.to_bytes().unwrap();
            assert_eq!(bytes.len(), expected_size);
            assert!(bytes.len() <= MAX_TIP_RESPONSE_BINCODE_SIZE);
            assert_eq!(response.to_bounded_bytes().unwrap().as_slice(), bytes);
            assert!(matches!(
                TipResponse::from_bytes(&bytes).unwrap(),
                TipResponse::Failure(decoded) if decoded == reason
            ));
        }
    }
}
