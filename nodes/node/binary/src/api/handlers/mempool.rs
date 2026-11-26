use axum::{Json, extract::State, response::Response};
use lb_api_common::paths;
use lb_core::mantle::{TxHash, traits::Hashable};

use crate::{
    ValidatedTx,
    api::{
        calls::{Handle, mempool},
        errors::ErrorBody,
        openapi::schema,
    },
};

#[utoipa::path(
    post,
    path = paths::MEMPOOL_ADD_TX,
    responses(
        (status = 200, description = "Add transaction to the mempool"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn add_tx(State(handle): State<Handle>, Json(tx): Json<ValidatedTx>) -> Response {
    make_request_and_return_response!(mempool::add_tx(&handle, tx))
}

#[utoipa::path(
    get,
    path = paths::MEMPOOL_VIEW,
    responses(
        (status = 200, description = "Get current tip mempool transaction hashes", body = Vec<TxHash>),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn mempool_view(State(handle): State<Handle>) -> Response {
    make_request_and_return_response!(mempool::view(&handle))
}

#[utoipa::path(
    get,
    path = paths::MANTLE_METRICS,
    responses(
        (status = 200, description = "Get the mempool metrics of the cl service", body = inline(schema::MempoolMetrics)),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn mantle_metrics(State(handle): State<Handle>) -> Response {
    make_request_and_return_response!(mempool::metrics(&handle))
}

#[utoipa::path(
    post,
    path = paths::MANTLE_STATUS,
    responses(
        (status = 200, description = "Query the mempool status of the cl service", body = Vec<TxHash>),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn mantle_status(
    State(handle): State<Handle>,
    Json(items): Json<Vec<<ValidatedTx as Hashable>::Hash>>,
) -> Response {
    make_request_and_return_response!(mempool::status(&handle, items))
}
