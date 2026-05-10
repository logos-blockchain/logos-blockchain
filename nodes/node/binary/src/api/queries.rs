use std::num::{NonZero, NonZeroUsize};

use lb_http_api_common::{
    DEFAULT_BLOCKS_STREAM_CHUNK_SIZE, DEFAULT_NUMBER_OF_BLOCKS_TO_STREAM, MAX_BLOCKS_STREAM_BLOCKS,
    MAX_BLOCKS_STREAM_CHUNK_SIZE,
};
use serde::Deserialize;
use utoipa::IntoParams;
use validator::Validate;

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

/// Query parameters for the blocks stream endpoint, with validation and
/// `OpenAPI` schema generation. Note: Literals in `param` are duplicated due to
/// utoipa attribute limitations.
#[derive(IntoParams, Deserialize, Validate)]
#[into_params(parameter_in = Query)]
pub struct BlocksStreamQuery {
    /// If omitted, the server chooses a default lower bound.
    /// For descending streams this is `slot 0` (bounded by `blocks_limit`).
    /// For ascending streams, `slot_from` is estimated from the average
    /// slots-per-block and `blocks_limit`, biased so the stream ends near
    /// `slot_to`. This may return fewer than `blocks_limit` blocks; callers
    /// can refine by specifying `slot_from` explicitly.
    #[serde(default)]
    #[param(minimum = 0)]
    pub slot_from: Option<u64>,
    /// Upper bound slot (inclusive). Defaults to tip slot, or LIB slot when
    /// `immutable_only=true`.
    #[serde(default)]
    #[param(minimum = 0)]
    pub slot_to: Option<u64>,
    /// Sort direction. Defaults to descending (`true`).
    #[serde(default)]
    pub descending: Option<bool>,
    /// The maximum number of actual blocks to return. If omitted:
    /// - explicit bounded slot range (`slot_from` and `slot_to`) defaults to
    ///   the server maximum (`630_720_000`);
    /// - otherwise defaults to `100`.
    #[serde(default)]
    #[validate(custom(function = "validate_blocks_limit"))]
    #[param(minimum = 1, maximum = 630_720_000, default = 100, example = 100)]
    pub blocks_limit: Option<usize>,
    /// Server chunk size hint for streamed delivery. Defaults to `100` ,
    /// maximum `1000`.
    #[serde(default)]
    #[validate(custom(function = "validate_server_batch_size"))]
    #[param(minimum = 1, maximum = 1_000, default = 100, example = 100)]
    pub server_batch_size: Option<usize>,
    /// When true, include only immutable blocks.
    /// If `slot_to` is omitted, the default anchor is LIB slot.
    #[serde(default)]
    pub immutable_only: Option<bool>,
}

fn validate_blocks_limit(v: usize) -> Result<(), validator::ValidationError> {
    if v == 0 || v > MAX_BLOCKS_STREAM_BLOCKS {
        let mut err = validator::ValidationError::new("out_of_range");
        err.message =
            Some(format!("'blocks_limit' must be in [1, {MAX_BLOCKS_STREAM_BLOCKS}]").into());
        err.add_param("field".into(), &"blocks_limit");
        err.add_param("min".into(), &1);
        err.add_param("max".into(), &MAX_BLOCKS_STREAM_BLOCKS);
        err.add_param("value".into(), &v);
        return Err(err);
    }
    Ok(())
}

fn validate_server_batch_size(v: usize) -> Result<(), validator::ValidationError> {
    if v == 0 || v > MAX_BLOCKS_STREAM_CHUNK_SIZE {
        let mut err = validator::ValidationError::new("out_of_range");
        err.message = Some(
            format!("'server_batch_size' must be in [1, {MAX_BLOCKS_STREAM_CHUNK_SIZE}]").into(),
        );
        err.add_param("field".into(), &"server_batch_size");
        err.add_param("min".into(), &1);
        err.add_param("max".into(), &MAX_BLOCKS_STREAM_CHUNK_SIZE);
        err.add_param("value".into(), &v);
        return Err(err);
    }
    Ok(())
}

impl TryFrom<BlocksStreamQuery> for BlocksStreamRequest {
    type Error = BlocksStreamRequestError;

    // Parse and validate the query parameters for the blocks stream endpoint,
    // applying defaults where necessary.
    fn try_from(query: BlocksStreamQuery) -> Result<Self, Self::Error> {
        query.validate()?;

        let blocks_limit = query.blocks_limit.unwrap_or_else(|| {
            if query.slot_from.is_some() && query.slot_to.is_some() {
                MAX_BLOCKS_STREAM_BLOCKS
            } else {
                DEFAULT_NUMBER_OF_BLOCKS_TO_STREAM
            }
        });

        let server_batch_size = query
            .server_batch_size
            .unwrap_or(DEFAULT_BLOCKS_STREAM_CHUNK_SIZE);

        if let (Some(slot_from), Some(slot_to)) = (query.slot_from, query.slot_to)
            && slot_from > slot_to
        {
            return Err(BlocksStreamRequestError::InvalidSlotRange { slot_from, slot_to });
        }

        Ok(Self {
            slot_from: query.slot_from,
            slot_to: query.slot_to,
            descending: query.descending.unwrap_or(true),
            blocks_limit: NonZeroUsize::new(blocks_limit)
                .expect("'blocks_limit' is always >= 1 in schema"),
            server_batch_size: NonZeroUsize::new(server_batch_size)
                .expect("'server_batch_size' is always >= 1 in schema"),
            immutable_only: query.immutable_only.unwrap_or_default(),
        })
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_stream_request_defaults_to_recent_blocks_from_tip() {
        let request = BlocksStreamRequest::try_from(BlocksStreamQuery {
            slot_from: None,
            slot_to: None,
            descending: None,
            blocks_limit: None,
            server_batch_size: None,
            immutable_only: None,
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
            descending: None,
            blocks_limit: Some(7),
            server_batch_size: None,
            immutable_only: None,
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
            descending: Some(true),
            blocks_limit: None,
            server_batch_size: None,
            immutable_only: None,
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
    fn blocks_stream_request_rejects_zero_limit() {
        let result = BlocksStreamRequest::try_from(BlocksStreamQuery {
            slot_from: None,
            slot_to: None,
            descending: None,
            blocks_limit: Some(0),
            server_batch_size: None,
            immutable_only: None,
        });

        match result.err() {
            Some(BlocksStreamRequestError::Validation { .. }) => {}
            _ => panic!("Expected validation error for 'blocks_limit'"),
        }
    }

    #[test]
    fn blocks_stream_request_rejects_zero_batch_size() {
        let result = BlocksStreamRequest::try_from(BlocksStreamQuery {
            slot_from: None,
            slot_to: None,
            descending: None,
            blocks_limit: None,
            server_batch_size: Some(0),
            immutable_only: None,
        });

        match result.err() {
            Some(BlocksStreamRequestError::Validation { .. }) => {}
            _ => panic!("Expected validation error for 'server_batch_size'"),
        }
    }

    #[test]
    fn blocks_stream_request_rejects_invalid_slot_range() {
        let result = BlocksStreamRequest::try_from(BlocksStreamQuery {
            slot_from: Some(9),
            slot_to: Some(7),
            descending: None,
            blocks_limit: None,
            server_batch_size: None,
            immutable_only: None,
        });

        match result.err() {
            Some(BlocksStreamRequestError::InvalidSlotRange { .. }) => {}
            _ => panic!("Expected validation error for 'slot_from' > 'slot_to'"),
        }
    }
}
