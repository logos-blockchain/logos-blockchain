use std::num::NonZero;

use serde::{Deserialize, Serialize};
use url::Url;
use utoipa::IntoParams;
use validator::Validate;

use crate::{MAX_BLOCKS_STREAM_BLOCKS, MAX_BLOCKS_STREAM_CHUNK_SIZE};

/// Query parameters for the blocks stream endpoint, with validation and
/// `OpenAPI` schema generation. Note: Literals in `param` are duplicated due to
/// utoipa attribute limitations.
#[derive(Debug, Copy, Clone, PartialEq, Eq, IntoParams, Deserialize, Serialize, Validate)]
#[into_params(parameter_in = Query)]
pub struct BlocksStreamQuery {
    /// If omitted, the server chooses a default lower bound.
    /// For descending streams this is `slot 0` (bounded by `blocks_limit`).
    /// For ascending streams, `slot_from` is estimated from the average
    /// slots-per-block and `blocks_limit`, biased so the stream ends near
    /// `slot_to`. This may return fewer than `blocks_limit` blocks; callers
    /// can refine by specifying `slot_from` explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[param(minimum = 0)]
    pub slot_from: Option<u64>,
    /// Upper bound slot (inclusive). Defaults to tip slot, or LIB slot when
    /// `immutable_only=true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[param(minimum = 0)]
    pub slot_to: Option<u64>,
    /// Sort direction. Defaults to descending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<BlockSortOrder>,
    /// The maximum number of actual blocks to return. If omitted:
    /// - explicit bounded slot range (`slot_from` and `slot_to`) defaults to
    ///   the server maximum (`630_720_000`);
    /// - otherwise defaults to `100`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(custom(function = "validate_blocks_limit"))]
    #[param(minimum = 1, maximum = 630_720_000, default = 100, example = 100)]
    pub blocks_limit: Option<NonZero<usize>>,
    /// Server chunk size hint for streamed delivery. Defaults to `100` ,
    /// maximum `1000`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(custom(function = "validate_server_batch_size"))]
    #[param(minimum = 1, maximum = 1_000, default = 100, example = 100)]
    pub server_batch_size: Option<NonZero<usize>>,
    /// When true, include only immutable blocks.
    /// If `slot_to` is omitted, the default anchor is LIB slot.
    /// Defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_filter: Option<BlockFilter>,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "validator derive calls custom validators by reference"
)]
fn validate_blocks_limit(v: &NonZero<usize>) -> Result<(), validator::ValidationError> {
    if v.get() > MAX_BLOCKS_STREAM_BLOCKS {
        let mut err = validator::ValidationError::new("out_of_range");
        err.message =
            Some(format!("'blocks_limit' must be in [1, {MAX_BLOCKS_STREAM_BLOCKS}]").into());
        err.add_param("field".into(), &"blocks_limit");
        err.add_param("min".into(), &1);
        err.add_param("max".into(), &MAX_BLOCKS_STREAM_BLOCKS);
        err.add_param("value".into(), &v.get());
        return Err(err);
    }
    Ok(())
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "validator derive calls custom validators by reference"
)]
fn validate_server_batch_size(v: &NonZero<usize>) -> Result<(), validator::ValidationError> {
    if v.get() > MAX_BLOCKS_STREAM_CHUNK_SIZE {
        let mut err = validator::ValidationError::new("out_of_range");
        err.message = Some(
            format!("'server_batch_size' must be in [1, {MAX_BLOCKS_STREAM_CHUNK_SIZE}]").into(),
        );
        err.add_param("field".into(), &"server_batch_size");
        err.add_param("min".into(), &1);
        err.add_param("max".into(), &MAX_BLOCKS_STREAM_CHUNK_SIZE);
        err.add_param("value".into(), &v.get());
        return Err(err);
    }
    Ok(())
}

impl BlocksStreamQuery {
    const fn params_is_none(&self) -> bool {
        self.slot_from.is_none()
            && self.slot_to.is_none()
            && self.order.is_none()
            && self.blocks_limit.is_none()
            && self.server_batch_size.is_none()
            && self.block_filter.is_none()
    }

    /// Append query parameters to the given URL.
    ///
    /// Fields that are `None` are omitted so server defaults apply.
    pub fn append_to_url(&self, request_url: &mut Url) {
        if self.params_is_none() {
            return;
        }

        if let Ok(encoded) = serde_urlencoded::to_string(self) {
            request_url.query_pairs_mut().extend_pairs(
                url::form_urlencoded::parse(encoded.as_bytes())
                    .map(|(k, v)| (k.into_owned(), v.into_owned())),
            );
        }
    }
}

/// Sort order for blocks in the `get_blocks_range_stream` method.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockSortOrder {
    /// Ascending order (oldest to newest).
    Ascending,
    /// Descending order (newest to oldest).
    Descending,
}

/// Filter for block types in the `get_blocks_range_stream` method.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockFilter {
    /// Includes only immutable blocks.
    ImmutableOnly,
    /// Includes mutable and immutable blocks.
    MutableAndImmutable,
}

#[cfg(test)]
mod tests {
    use std::num::NonZero;

    use url::Url;

    use crate::queries::{BlockFilter, BlockSortOrder, BlocksStreamQuery};

    #[test]
    fn blocks_stream_query_encodes_to_server_query_shape() {
        let mut request_url = Url::parse("http://localhost:8080/cryptarchia/blocks_range").unwrap();

        let params = BlocksStreamQuery {
            slot_from: Some(10),
            slot_to: Some(20),
            order: Some(BlockSortOrder::Ascending),
            blocks_limit: NonZero::new(50),
            server_batch_size: NonZero::new(5),
            block_filter: Some(BlockFilter::ImmutableOnly),
        };
        params.append_to_url(&mut request_url);

        assert_eq!(
            request_url.query(),
            Some(
                "slot_from=10&slot_to=20&order=ascending&blocks_limit=50&server_batch_size=5&block_filter=immutable_only"
            )
        );

        let params = BlocksStreamQuery {
            slot_from: Some(10),
            slot_to: Some(20),
            order: Some(BlockSortOrder::Descending),
            blocks_limit: NonZero::new(50),
            server_batch_size: NonZero::new(5),
            block_filter: Some(BlockFilter::MutableAndImmutable),
        };
        let mut request_url = Url::parse("http://localhost:8080/cryptarchia/blocks_range").unwrap();
        params.append_to_url(&mut request_url);

        assert_eq!(
            request_url.query(),
            Some(
                "slot_from=10&slot_to=20&order=descending&blocks_limit=50&server_batch_size=5&block_filter=mutable_and_immutable"
            )
        );
    }

    #[test]
    fn blocks_stream_query_omits_unspecified_values() {
        let mut request_url = Url::parse("http://localhost:8080/cryptarchia/blocks_range").unwrap();

        let params = BlocksStreamQuery {
            slot_from: None,
            slot_to: None,
            order: None,
            blocks_limit: None,
            server_batch_size: None,
            block_filter: None,
        };
        params.append_to_url(&mut request_url);

        assert_eq!(
            request_url.as_str(),
            "http://localhost:8080/cryptarchia/blocks_range"
        );
        assert!(request_url.query().is_none());
    }
}
