use std::num::NonZero;

use futures::StreamExt as _;
use lb_common_http_client::{ApiBlock, Events};
use lb_http_api_common::queries::{BlockFilter, BlockSortOrder, BlocksStreamQuery};
use lb_testing_framework::NodeHttpClient;

use super::error::ScannerError;

/// Stream non-genesis blocks in ascending slot order and handle each block
/// immediately.
pub async fn stream_blocks_range(
    client: &NodeHttpClient,
    slot_from: u64,
    slot_to: u64,
    mut on_block: impl FnMut(ApiBlock, Events) -> Result<(), ScannerError>,
) -> Result<usize, ScannerError> {
    if slot_from > slot_to {
        return Ok(0);
    }

    let mut stream = client
        .blocks_range_stream(BlocksStreamQuery {
            slot_from: Some(slot_from),
            slot_to: Some(slot_to),
            order: Some(BlockSortOrder::Ascending),
            blocks_limit: None,
            server_batch_size: NonZero::new(1000),
            block_filter: Some(BlockFilter::MutableAndImmutable),
        })
        .await?;

    let genesis_parent = lb_core::header::HeaderId::from([0; 32]);
    let mut streamed_blocks = 0usize;

    while let Some(event) = stream.next().await {
        if event.block.header.parent_block == genesis_parent {
            continue;
        }

        streamed_blocks += 1;
        let block_id = event.block.header.id;
        let block_events = client
            .block_events(&block_id)
            .await?
            .ok_or(ScannerError::MissingBlockEvents(block_id))?;
        on_block(event.block, block_events)?;
    }

    Ok(streamed_blocks)
}
