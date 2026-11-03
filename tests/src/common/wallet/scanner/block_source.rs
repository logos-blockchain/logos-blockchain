use std::num::NonZero;

use futures::StreamExt as _;
use lb_common_http_client::ApiBlock;
use lb_core::{events::Events, header::HeaderId};
use lb_http_api_common::{
    paths::BLOCK_EVENTS,
    queries::{BlockFilter, BlockSortOrder, BlocksStreamQuery},
};
use lb_testing_framework::NodeHttpClient;

use super::error::ScannerError;

/// Stream non-genesis blocks in ascending slot order and handle each block
/// immediately, together with the events of its execution.
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

    let genesis_parent = HeaderId::from([0; 32]);
    let mut streamed_blocks = 0usize;

    while let Some(event) = stream.next().await {
        if event.block.header.parent_block == genesis_parent {
            continue;
        }

        streamed_blocks += 1;
        let events = block_events(client, event.block.header.id).await?;
        on_block(event.block, events)?;
    }

    Ok(streamed_blocks)
}

// The events of a block's execution, which report what its note sets inserted
// and appended.
pub async fn block_events(client: &NodeHttpClient, id: HeaderId) -> Result<Events, ScannerError> {
    let path = BLOCK_EVENTS
        .trim_start_matches('/')
        .replace(":id", &id.to_string());
    let url = client
        .base_url()
        .join(&path)
        .map_err(|error| ScannerError::Logical(format!("block events URL: {error}")))?;
    let response = reqwest::get(url)
        .await
        .map_err(|error| ScannerError::Logical(format!("block events request: {error}")))?;
    if !response.status().is_success() {
        return Err(ScannerError::Logical(format!(
            "block events of {id} answered {}",
            response.status()
        )));
    }
    response
        .json::<Events>()
        .await
        .map_err(|error| ScannerError::Logical(format!("block events response: {error}")))
}
