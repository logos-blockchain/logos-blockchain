use std::num::{NonZero, NonZeroUsize};

use lb_http_api_common::{
    DEFAULT_BLOCKS_STREAM_CHUNK_SIZE, DEFAULT_NUMBER_OF_BLOCKS_TO_STREAM, MAX_BLOCKS_STREAM_BLOCKS,
    queries::{BlockFilter, BlockSortOrder, BlocksStreamQuery},
};
use serde::Deserialize;
use utoipa::IntoParams;
use validator::Validate as _;

use crate::api::errors::BlocksStreamRequestError;

#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[derive(Deserialize)]
pub struct BlockRangeQuery {
    #[param(minimum = 0)]
    pub slot_from: usize,
    #[param(minimum = 0)]
    pub slot_to: usize,
}

/// This is a processed `BlocksStreamQuery` with all defaults applied and
/// validated, ready to be used for fetching blocks.
pub struct BlocksStreamRequest {
    /// An optional lower bound slot.
    pub slot_from: Option<u64>,
    /// An optional upper bound slot (inclusive).
    pub slot_to: Option<u64>,
    /// Sort direction, either descending or the opposite.
    pub descending: bool,
    /// Maximum number of blocks to return.
    pub blocks_limit: NonZero<usize>,
    /// Server chunk size hint for streamed delivery.
    pub server_batch_size: NonZero<usize>,
    /// When true, include only immutable blocks.
    pub immutable_only: bool,
}

impl TryFrom<BlocksStreamQuery> for BlocksStreamRequest {
    type Error = BlocksStreamRequestError;

    // Parse and validate the query parameters for the blocks stream endpoint,
    // applying defaults where necessary.
    fn try_from(query: BlocksStreamQuery) -> Result<Self, Self::Error> {
        query.validate()?;

        let blocks_limit = query.blocks_limit.unwrap_or_else(|| {
            if query.slot_from.is_some() && query.slot_to.is_some() {
                NonZeroUsize::new(MAX_BLOCKS_STREAM_BLOCKS).unwrap()
            } else {
                NonZeroUsize::new(DEFAULT_NUMBER_OF_BLOCKS_TO_STREAM).unwrap()
            }
        });

        let server_batch_size = query
            .server_batch_size
            .unwrap_or(NonZeroUsize::new(DEFAULT_BLOCKS_STREAM_CHUNK_SIZE).unwrap());

        if let (Some(slot_from), Some(slot_to)) = (query.slot_from, query.slot_to)
            && slot_from > slot_to
        {
            return Err(BlocksStreamRequestError::InvalidSlotRange { slot_from, slot_to });
        }

        Ok(Self {
            slot_from: query.slot_from,
            slot_to: query.slot_to,
            descending: query.order.unwrap_or(BlockSortOrder::Descending)
                == BlockSortOrder::Descending,
            blocks_limit,
            server_batch_size,
            immutable_only: query
                .block_filter
                .unwrap_or(BlockFilter::MutableAndImmutable)
                == BlockFilter::ImmutableOnly,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::num::{NonZero, NonZeroUsize};

    use lb_http_api_common::{
        DEFAULT_BLOCKS_STREAM_CHUNK_SIZE, DEFAULT_NUMBER_OF_BLOCKS_TO_STREAM,
        MAX_BLOCKS_STREAM_BLOCKS,
        queries::{BlockFilter, BlockSortOrder, BlocksStreamQuery},
    };

    use crate::api::{errors::BlocksStreamRequestError, queries::BlocksStreamRequest};

    #[test]
    fn blocks_stream_request_defaults_to_recent_blocks_from_tip() {
        let request = BlocksStreamRequest::try_from(BlocksStreamQuery {
            slot_from: None,
            slot_to: None,
            order: None,
            blocks_limit: None,
            server_batch_size: None,
            block_filter: None,
        })
        .expect("query without explicit range should parse");

        assert_eq!(
            request.blocks_limit,
            NonZero::new(DEFAULT_NUMBER_OF_BLOCKS_TO_STREAM).unwrap()
        );
        assert_eq!(request.slot_from, None);
        assert_eq!(request.slot_to, None);
        assert!(request.descending);
        assert_eq!(
            request.server_batch_size,
            NonZero::new(DEFAULT_BLOCKS_STREAM_CHUNK_SIZE).unwrap()
        );
        assert!(!request.immutable_only);
    }

    #[test]
    fn blocks_stream_request_accepts_blocks_limit_only() {
        let request = BlocksStreamRequest::try_from(BlocksStreamQuery {
            slot_from: None,
            slot_to: None,
            order: None,
            blocks_limit: NonZeroUsize::new(7),
            server_batch_size: None,
            block_filter: None,
        })
        .expect("query with explicit blocks_limit should parse");

        assert_eq!(request.blocks_limit, NonZero::new(7).unwrap());
        assert_eq!(request.slot_to, None);
    }

    #[test]
    fn blocks_stream_request_accepts_slot_range() {
        let request = BlocksStreamRequest::try_from(BlocksStreamQuery {
            slot_from: Some(10),
            slot_to: Some(20),
            order: Some(BlockSortOrder::Descending),
            blocks_limit: None,
            server_batch_size: None,
            block_filter: None,
        })
        .expect("query with explicit slot range should parse");

        assert_eq!(
            request.blocks_limit,
            NonZero::new(MAX_BLOCKS_STREAM_BLOCKS).unwrap()
        );
        assert_eq!(request.slot_from, Some(10));
        assert_eq!(request.slot_to, Some(20));
        assert!(request.descending);
    }

    #[test]
    fn blocks_stream_request_maps_typed_order_and_filter() {
        let request = BlocksStreamRequest::try_from(BlocksStreamQuery {
            slot_from: Some(10),
            slot_to: Some(20),
            order: Some(BlockSortOrder::Ascending),
            blocks_limit: NonZeroUsize::new(7),
            server_batch_size: NonZeroUsize::new(3),
            block_filter: Some(BlockFilter::ImmutableOnly),
        })
        .expect("typed query should parse");

        assert!(!request.descending);
        assert_eq!(request.blocks_limit, NonZero::new(7).unwrap());
        assert_eq!(request.server_batch_size, NonZero::new(3).unwrap());
        assert!(request.immutable_only);
    }

    #[test]
    fn blocks_stream_query_rejects_zero_limit_at_deserialization() {
        let result = serde_urlencoded::from_str::<BlocksStreamQuery>("blocks_limit=0");

        assert!(
            result.is_err(),
            "zero blocks_limit should fail to deserialize"
        );
    }

    #[test]
    fn blocks_stream_query_rejects_zero_batch_size_at_deserialization() {
        let result = serde_urlencoded::from_str::<BlocksStreamQuery>("server_batch_size=0");

        assert!(
            result.is_err(),
            "zero server_batch_size should fail to deserialize"
        );
    }

    #[test]
    fn blocks_stream_request_rejects_invalid_slot_range() {
        let result = BlocksStreamRequest::try_from(BlocksStreamQuery {
            slot_from: Some(9),
            slot_to: Some(7),
            order: None,
            blocks_limit: None,
            server_batch_size: None,
            block_filter: None,
        });

        match result.err() {
            Some(BlocksStreamRequestError::InvalidSlotRange { .. }) => {}
            _ => panic!("Expected validation error for 'slot_from' > 'slot_to'"),
        }
    }
}
