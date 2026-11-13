use axum::{Json, extract::State, response::Response};
use lb_core::mantle::TxHash;
use lb_http_api_common::{bodies::blend::JoinBlendRequestBody, paths};

use crate::{
    ValidatedTx,
    api::{
        calls::{Handle, blend},
        errors::ErrorBody,
    },
};

#[utoipa::path(
    get,
    path = paths::BLEND_NETWORK_INFO,
    responses(
        (status = 200, description = "Query the blend network information", body = Option<Object>),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn blend_info(State(handle): State<Handle>) -> Response {
    make_request_and_return_response!(blend::info(&handle))
}

#[utoipa::path(
    post,
    path = paths::BLEND_JOIN_NETWORK,
    request_body = JoinBlendRequestBody,
    responses(
        (status = 200, description = "Join the blend network", body = lb_sdp_service::SdpSubmission),
        (status = 404, description = "No eligible note on this node's blend ZK key", body = ErrorBody),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn blend_join_network(
    State(handle): State<Handle>,
    Json(req): Json<JoinBlendRequestBody>,
) -> Response {
    make_request_and_return_response!(blend::join_network(
        &handle,
        req.locator,
        req.service_note_id
    ))
}

#[utoipa::path(
    get,
    path = paths::BLEND_PENDING_TRANSACTIONS,
    responses(
        (status = 200, description = "Ids of the transactions waiting for a PoW solution before they can be blended", body = Vec<TxHash>),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn blend_pending_transactions(State(handle): State<Handle>) -> Response {
    make_request_and_return_response!(blend::pending_transactions(&handle))
}

#[utoipa::path(
    post,
    path = paths::BLEND_DISPERSE_TRANSACTION,
    responses(
        (status = 200, description = "Id of the transaction accepted for blending, which was not added to this node's mempool", body = TxHash),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn blend_tx(State(handle): State<Handle>, Json(tx): Json<ValidatedTx>) -> Response {
    make_request_and_return_response!(blend::disperse_transaction(&handle, tx))
}
