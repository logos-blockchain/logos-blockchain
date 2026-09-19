use std::collections::HashSet;

use lb_binary_codec::bincode::{self, BoundedSerializeOp, UpperBoundedVec};
use lb_core::{
    block::{BlockTransactions, MAX_BLOCK_TRANSACTIONS_SIZE},
    header::HeaderId,
};
use lb_cryptarchia_engine::MAX_UNCLES;
use lb_key_management_system_keys::keys::Ed25519Signature;
use serde::{Deserialize, Deserializer, Serialize, de::Visitor};

use crate::{
    BlocksUnavailableReason, GetTipResponse, SerialisedBlock,
    libp2p::provider::MAX_ADDITIONAL_BLOCKS,
};

/// Maximum configured-bincode size of a request, including five additional
/// known block identifiers.
pub const MAX_REQUEST_MESSAGE_BINCODE_SIZE: usize = bincode::BINCODE_ENUM_DISCRIMINANT_SIZE
    + 3 * <HeaderId as BoundedSerializeOp>::MAX_ENCODED_SIZE
    + bincode::BINCODE_LENGTH_PREFIX_SIZE
    + MAX_ADDITIONAL_BLOCKS * <HeaderId as BoundedSerializeOp>::MAX_ENCODED_SIZE;

/// Maximum configured-bincode size of one stored block. The block stores each
/// transaction as its canonical bytes inside a bincode byte envelope, so the
/// existing total transaction-content and transaction-count limits account for
/// all variable-sized block data.
pub const MAX_SERIALISED_BLOCK_BINCODE_SIZE: usize =
    <lb_core::header::Header as BoundedSerializeOp>::MAX_ENCODED_SIZE
        + <Ed25519Signature as BoundedSerializeOp>::MAX_ENCODED_SIZE
        + bincode::BINCODE_LENGTH_PREFIX_SIZE
        + MAX_UNCLES
            * (<lb_core::header::Header as BoundedSerializeOp>::MAX_ENCODED_SIZE
                + <Ed25519Signature as BoundedSerializeOp>::MAX_ENCODED_SIZE)
        + bincode::BINCODE_LENGTH_PREFIX_SIZE
        + MAX_BLOCK_TRANSACTIONS_SIZE
        + BlockTransactions::<()>::MAX * bincode::BINCODE_LENGTH_PREFIX_SIZE;

/// Maximum configured-bincode size of a `DownloadBlocksResponse` frame.
pub const MAX_DOWNLOAD_BLOCKS_RESPONSE_BINCODE_SIZE: usize = bincode::BINCODE_ENUM_DISCRIMINANT_SIZE
    + bincode::BINCODE_LENGTH_PREFIX_SIZE
    + MAX_SERIALISED_BLOCK_BINCODE_SIZE;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum RequestMessage {
    /// A request to download blocks.
    DownloadBlocksRequest(DownloadBlocksRequest),
    /// A request to get the tip of the peer.
    GetTip,
}

impl BoundedSerializeOp for RequestMessage {
    type Bytes = UpperBoundedVec<u8, MAX_REQUEST_MESSAGE_BINCODE_SIZE>;
}

/// A request to initiate block downloading from a peer.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DownloadBlocksRequest {
    /// Return blocks up to `target_block`.
    pub target_block: HeaderId,
    /// The list of known blocks that the requester has.
    pub known_blocks: KnownBlocks,
}

/// A set of block identifiers the syncing peer already knows.
#[derive(Debug, Serialize, Clone)]
pub struct KnownBlocks {
    /// The local canonical chain latest block.
    pub local_tip: HeaderId,
    /// The latest immutable block.
    pub latest_immutable_block: HeaderId,
    /// The list of additional blocks that the requester has.
    pub additional_blocks: HashSet<HeaderId>,
}

impl<'de> Deserialize<'de> for KnownBlocks {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawKnownBlocks {
            local_tip: HeaderId,
            latest_immutable_block: HeaderId,
            #[serde(deserialize_with = "deserialize_additional_blocks")]
            additional_blocks: Vec<HeaderId>,
        }

        let raw = RawKnownBlocks::deserialize(deserializer)?;
        let mut additional_blocks = HashSet::with_capacity(raw.additional_blocks.len());
        for block_id in raw.additional_blocks {
            if !additional_blocks.insert(block_id) {
                return Err(serde::de::Error::custom(
                    "additional_blocks contains duplicate block identifiers",
                ));
            }
        }

        Ok(Self {
            local_tip: raw.local_tip,
            latest_immutable_block: raw.latest_immutable_block,
            additional_blocks,
        })
    }
}

fn deserialize_additional_blocks<'de, D>(deserializer: D) -> Result<Vec<HeaderId>, D::Error>
where
    D: Deserializer<'de>,
{
    struct AdditionalBlocksVisitor;

    impl<'de> Visitor<'de> for AdditionalBlocksVisitor {
        type Value = Vec<HeaderId>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(&format!(
                "at most {MAX_ADDITIONAL_BLOCKS} additional block identifiers"
            ))
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut block_ids = Vec::with_capacity(MAX_ADDITIONAL_BLOCKS);
            while let Some(block_id) = sequence.next_element()? {
                if block_ids.len() == MAX_ADDITIONAL_BLOCKS {
                    return Err(serde::de::Error::custom(format_args!(
                        "additional_blocks exceeds maximum of {MAX_ADDITIONAL_BLOCKS} entries"
                    )));
                }
                block_ids.push(block_id);
            }
            Ok(block_ids)
        }
    }

    deserializer.deserialize_seq(AdditionalBlocksVisitor)
}

impl DownloadBlocksRequest {
    #[must_use]
    pub const fn new(
        target_block: HeaderId,
        local_tip: HeaderId,
        latest_immutable_block: HeaderId,
        additional_blocks: HashSet<HeaderId>,
    ) -> Self {
        Self {
            target_block,
            known_blocks: KnownBlocks {
                local_tip,
                latest_immutable_block,
                additional_blocks,
            },
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum DownloadBlocksResponse {
    /// A response containing a block.
    Block(SerialisedBlock),
    /// A response indicating that no more blocks are available.
    NoMoreBlocks,
    /// A response indicating that the request failed.
    Failure(BlocksUnavailableReason),
}

impl BoundedSerializeOp for DownloadBlocksResponse {
    type Bytes = UpperBoundedVec<u8, MAX_DOWNLOAD_BLOCKS_RESPONSE_BINCODE_SIZE>;
}

// These compile-time guards justify removing the former shared Chain Sync
// admission ceiling without making it runtime policy again.
const _: () = {
    assert!(
        <RequestMessage as BoundedSerializeOp>::MAX_ENCODED_SIZE
            <= lb_utils::net::MAX_WIRE_MESSAGE_SIZE
    );
    assert!(
        <GetTipResponse as BoundedSerializeOp>::MAX_ENCODED_SIZE
            <= lb_utils::net::MAX_WIRE_MESSAGE_SIZE
    );
    assert!(
        <DownloadBlocksResponse as BoundedSerializeOp>::MAX_ENCODED_SIZE
            <= lb_utils::net::MAX_WIRE_MESSAGE_SIZE
    );
};

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use lb_binary_codec::bincode::{
        self, BoundedSerializeOp, DeserializeOp as _, SerializeOp as _,
    };
    use lb_core::header::HeaderId;

    use super::{
        DownloadBlocksRequest, DownloadBlocksResponse, KnownBlocks,
        MAX_DOWNLOAD_BLOCKS_RESPONSE_BINCODE_SIZE, MAX_REQUEST_MESSAGE_BINCODE_SIZE,
        MAX_SERIALISED_BLOCK_BINCODE_SIZE, RequestMessage,
    };
    use crate::BlocksUnavailableReason;

    #[test]
    fn known_blocks_rejects_more_than_maximum_encoded_entries() {
        let request = DownloadBlocksRequest::new(
            HeaderId::from([0; 32]),
            HeaderId::from([1; 32]),
            HeaderId::from([2; 32]),
            (0..=5)
                .map(|index| HeaderId::from([index; 32]))
                .collect::<HashSet<_>>(),
        );

        assert!(DownloadBlocksRequest::from_bytes(&request.to_bytes().unwrap()).is_err());
    }

    #[test]
    fn request_bounded_serialization_rejects_more_than_maximum_entries() {
        let request = RequestMessage::DownloadBlocksRequest(DownloadBlocksRequest::new(
            HeaderId::from([0; 32]),
            HeaderId::from([1; 32]),
            HeaderId::from([2; 32]),
            (3..9)
                .map(|index| HeaderId::from([index; 32]))
                .collect::<HashSet<_>>(),
        ));

        assert!(request.to_bounded_bytes().is_err());
    }

    #[test]
    fn get_tip_bounded_serialization_preserves_its_bincode_bytes() {
        let request = RequestMessage::GetTip;
        let ordinary = request.to_bytes().unwrap();
        let bounded = request.to_bounded_bytes().unwrap();

        assert_eq!(ordinary.len(), bincode::BINCODE_ENUM_DISCRIMINANT_SIZE);
        assert_eq!(bounded.as_slice(), ordinary.as_ref());
    }

    #[test]
    fn known_blocks_rejects_duplicate_encoded_entries() {
        #[derive(serde::Serialize)]
        struct RawKnownBlocks {
            local_tip: HeaderId,
            latest_immutable_block: HeaderId,
            additional_blocks: Vec<HeaderId>,
        }

        let raw = RawKnownBlocks {
            local_tip: HeaderId::from([0; 32]),
            latest_immutable_block: HeaderId::from([1; 32]),
            additional_blocks: vec![HeaderId::from([2; 32]); 2],
        };
        let bytes = raw.to_bytes().unwrap();

        assert!(KnownBlocks::from_bytes(&bytes).is_err());
    }

    #[test]
    fn request_bound_covers_the_maximum_known_block_set() {
        let request = RequestMessage::DownloadBlocksRequest(DownloadBlocksRequest::new(
            HeaderId::from([0; 32]),
            HeaderId::from([1; 32]),
            HeaderId::from([2; 32]),
            (3..8)
                .map(|index| HeaderId::from([index; 32]))
                .collect::<HashSet<_>>(),
        ));
        let ordinary = request.to_bytes().unwrap();
        let bounded = request.to_bounded_bytes().unwrap();
        let ordinary: &[u8] = ordinary.as_ref();

        assert_eq!(ordinary.len(), MAX_REQUEST_MESSAGE_BINCODE_SIZE);
        assert_eq!(bounded.as_slice(), ordinary);
    }

    #[test]
    fn response_bound_includes_the_block_bincode_envelope() {
        let response = DownloadBlocksResponse::Block(bytes::Bytes::from(vec![
            0;
            MAX_SERIALISED_BLOCK_BINCODE_SIZE
        ]));
        let ordinary = response.to_bytes().unwrap();
        let ordinary: &[u8] = ordinary.as_ref();

        assert_eq!(ordinary.len(), MAX_DOWNLOAD_BLOCKS_RESPONSE_BINCODE_SIZE);
        let bounded = response.to_bounded_bytes().unwrap();
        assert_eq!(bounded.as_slice(), ordinary);
    }

    #[test]
    fn every_block_failure_reason_fits_the_response_bound() {
        let reasons = [
            (
                BlocksUnavailableReason::BlockNotFound(HeaderId::from([0; 32])),
                2 * bincode::BINCODE_ENUM_DISCRIMINANT_SIZE
                    + <HeaderId as BoundedSerializeOp>::MAX_ENCODED_SIZE,
            ),
            (
                BlocksUnavailableReason::StartBlockNotFound,
                2 * bincode::BINCODE_ENUM_DISCRIMINANT_SIZE,
            ),
            (
                BlocksUnavailableReason::Unknown,
                2 * bincode::BINCODE_ENUM_DISCRIMINANT_SIZE,
            ),
        ];

        for (reason, expected_size) in reasons {
            let expected = reason.clone();
            let response = DownloadBlocksResponse::Failure(reason);
            let ordinary = response.to_bytes().unwrap();
            let bounded = response.to_bounded_bytes().unwrap();

            assert_eq!(ordinary.len(), expected_size);
            assert!(ordinary.len() <= MAX_DOWNLOAD_BLOCKS_RESPONSE_BINCODE_SIZE);
            assert_eq!(bounded.as_slice(), ordinary.as_ref());

            match (
                expected,
                DownloadBlocksResponse::from_bytes(&ordinary).unwrap(),
            ) {
                (
                    BlocksUnavailableReason::BlockNotFound(expected),
                    DownloadBlocksResponse::Failure(BlocksUnavailableReason::BlockNotFound(actual)),
                ) => assert_eq!(expected, actual),
                (
                    BlocksUnavailableReason::StartBlockNotFound,
                    DownloadBlocksResponse::Failure(BlocksUnavailableReason::StartBlockNotFound),
                )
                | (
                    BlocksUnavailableReason::Unknown,
                    DownloadBlocksResponse::Failure(BlocksUnavailableReason::Unknown),
                ) => {}
                _ => panic!("block failure reason did not round-trip"),
            }
        }
    }
}
