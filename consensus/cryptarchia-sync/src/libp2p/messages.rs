use std::collections::HashSet;

use lb_core::header::HeaderId;
use serde::{Deserialize, Deserializer, Serialize, de::Visitor};

use crate::{BlocksUnavailableReason, SerialisedBlock, libp2p::provider::MAX_ADDITIONAL_BLOCKS};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum RequestMessage {
    /// A request to download blocks.
    DownloadBlocksRequest(DownloadBlocksRequest),
    /// A request to get the tip of the peer.
    GetTip,
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use lb_core::{
        codec::{DeserializeOp as _, SerializeOp as _},
        header::HeaderId,
    };

    use super::{DownloadBlocksRequest, KnownBlocks};

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
}
