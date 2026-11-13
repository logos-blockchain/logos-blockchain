use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    response::Response,
};
use lb_api_common::{
    bodies::pow::{PoWClaimRequestBody, PoWClaimResponseBody},
    paths,
};

use crate::api::{
    calls::{Handle, pow},
    errors::ErrorBody,
};

#[utoipa::path(
    put,
    path = paths::POW_START_MINING,
    responses(
        (status = 200, description = "PoW mining started"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn pow_start_mining(State(handle): State<Handle>) -> Response {
    make_request_and_return_response!(pow::start_mining(&handle))
}

#[utoipa::path(
    put,
    path = paths::POW_STOP_MINING,
    responses(
        (status = 200, description = "PoW mining stopped"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn pow_stop_mining(State(handle): State<Handle>) -> Response {
    make_request_and_return_response!(pow::stop_mining(&handle))
}

#[utoipa::path(
    put,
    path = paths::POW_START_AUTO_CLAIM,
    responses(
        (status = 200, description = "PoW auto-claim started"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn pow_start_auto_claim(State(handle): State<Handle>) -> Response {
    make_request_and_return_response!(pow::start_auto_claim(&handle))
}

#[utoipa::path(
    put,
    path = paths::POW_STOP_AUTO_CLAIM,
    responses(
        (status = 200, description = "PoW auto-claim stopped"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn pow_stop_auto_claim(State(handle): State<Handle>) -> Response {
    make_request_and_return_response!(pow::stop_auto_claim(&handle))
}

#[utoipa::path(
    post,
    path = paths::POW_CLAIM,
    request_body = Option<PoWClaimRequestBody>,
    responses(
        (status = 200, description = "PoW reward-claim transactions submitted", body = PoWClaimResponseBody),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn pow_claim(
    State(handle): State<Handle>,
    // An absent or empty body means "pay the auto-claim target", so the body
    // is optional rather than required.
    body: Result<Json<PoWClaimRequestBody>, JsonRejection>,
) -> Response {
    let request = body.map(|Json(request)| request).unwrap_or_default();
    make_request_and_return_response!(pow::claim(&handle, request))
}

#[utoipa::path(
    get,
    path = paths::POW_CLAIMABLE_REWARDS,
    responses(
        (status = 200, description = "PoW rewards this node can currently claim"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn pow_claimable_rewards(State(handle): State<Handle>) -> Response {
    make_request_and_return_response!(pow::claimable_rewards(&handle))
}

#[utoipa::path(
    get,
    path = paths::POW_STATUS,
    responses(
        (status = 200, description = "PoW mining and auto-claim state as the running node holds it"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn pow_status(State(handle): State<Handle>) -> Response {
    make_request_and_return_response!(pow::status(&handle))
}
