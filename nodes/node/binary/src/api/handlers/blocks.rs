use axum::{
    Json,
    extract::{Path, Query, State},
    response::{IntoResponse as _, Response},
};
use lb_api_common::{paths, queries::BlocksStreamQuery};
use lb_core::{header::HeaderId, mantle::TxHash};
use serde::Deserialize;
use tokio_stream::StreamExt as _;

use crate::api::{
    calls::{self, DynError, Handle, blocks, blocks_range},
    errors::ErrorBody,
    queries::BlockRangeQuery,
    responses::{self},
    serializers::{
        blocks::{ApiBlock, ApiBlockOwned, ApiProcessedBlockEventOwned},
        transactions::ApiSignedTransaction,
    },
};

#[derive(Deserialize)]
pub struct CryptarchiaInfoQuery {
    from: Option<HeaderId>,
    to: Option<HeaderId>,
}

#[utoipa::path(
    get,
    path = paths::CRYPTARCHIA_HEADERS,
    responses(
        (status = 200, description = "Query header ids", body = Vec<HeaderId>),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn cryptarchia_headers(
    State(handle): State<Handle>,
    Query(query): Query<CryptarchiaInfoQuery>,
) -> Response {
    let CryptarchiaInfoQuery { from, to } = query;
    make_request_and_return_response!(blocks::get_headers(&handle, from, to))
}

#[utoipa::path(
    get,
    path = paths::CRYPTARCHIA_LIB_STREAM,
    responses(
        (status = 200, description = "Request a stream for lib blocks"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn cryptarchia_lib_stream(State(handle): State<Handle>) -> Response {
    let stream = blocks::lib_block_stream(&handle).await;
    match stream {
        Ok(stream) => responses::ndjson::from_stream_result(stream),
        Err(error) => error.into_response(),
    }
}

#[utoipa::path(
    get,
    path = paths::BLOCKS,
    params(BlockRangeQuery),
    responses(
        (status = 200, description = "Get blocks"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn immutable_blocks(
    State(handle): State<Handle>,
    Query(query): Query<BlockRangeQuery>,
) -> Response {
    make_request_and_return_response!(async {
        let blocks = blocks::get_immutable_blocks(&handle, query.slot_from, query.slot_to).await?;
        Ok::<_, calls::Error>(
            blocks
                .into_iter()
                .map(ApiBlockOwned::from)
                .collect::<Vec<_>>(),
        )
    })
}

#[utoipa::path(
    get,
    path = paths::BLOCKS_DETAIL,
    responses(
        (status = 200, description = "Block found"),
        (status = 404, description = "Block not found", body = ErrorBody),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn block(State(handle): State<Handle>, Path(id): Path<HeaderId>) -> Response {
    match blocks::get_block(&handle, id).await {
        Ok(block) => Json(ApiBlock::from(&block)).into_response(),
        Err(error) => error.into_response(),
    }
}

#[utoipa::path(
    get,
    path = paths::BLOCK_EVENTS,
    responses(
        (status = 200, description = "Block events", body = Object),
        (status = 404, description = "Block not found", body = ErrorBody),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn block_events(State(handle): State<Handle>, Path(id): Path<HeaderId>) -> Response {
    make_request_and_return_response!(blocks::get_block_events(&handle, id))
}

#[utoipa::path(
    get,
    path = paths::BLOCKS_STREAM,
    responses(
        (status = 200, description = "Stream of processed blocks with chain state"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn blocks_stream(State(handle): State<Handle>) -> Response {
    let stream = blocks::get_new_blocks_stream(&handle)
        .await
        .map(|stream| stream.map(ApiProcessedBlockEventOwned::from));
    match stream {
        Ok(stream) => responses::ndjson::from_stream(stream),
        Err(error) => error.into_response(),
    }
}

#[utoipa::path(
    get,
    path = paths::BLOCKS_RANGE_STREAM,
    params(BlocksStreamQuery),
    responses(
        (status = 200, description = "Stream of processed blocks with chain state in slot order. \
            When immutable_only=true and slot_to is omitted, the stream anchors at LIB slot by \
            default."),
        (status = 400, description = "Invalid request parameters", body = ErrorBody),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn blocks_range_stream(
    State(handle): State<Handle>,
    Query(query): Query<BlocksStreamQuery>,
) -> Response {
    match blocks_range::stream(&handle, query).await {
        Ok(stream) => responses::ndjson::from_stream_result(stream.map(|block| {
            block
                .map(ApiProcessedBlockEventOwned::from)
                .map_err(DynError::from)
        })),
        Err(error) => error.into_response(),
    }
}

#[utoipa::path(
    get,
    path = paths::TRANSACTION,
    responses(
        (status = 200, description = "Transaction found"),
        (status = 404, description = "Transaction not found", body = ErrorBody),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn transaction(State(handle): State<Handle>, Path(id): Path<TxHash>) -> Response {
    match blocks::get_transaction(&handle, id).await {
        Ok(transaction) => Json(ApiSignedTransaction::from(&transaction)).into_response(),
        Err(error) => error.into_response(),
    }
}
