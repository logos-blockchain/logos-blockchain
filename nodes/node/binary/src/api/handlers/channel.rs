use axum::{
    Json,
    extract::{Path, State},
    response::Response,
};
use lb_core::mantle::ops::channel::ChannelId;
use lb_http_api_common::{bodies::channel::ChannelDepositRequestBody, paths};

use crate::api::{
    calls::{Handle, channel},
    errors::ErrorBody,
};

#[utoipa::path(
    get,
    path = paths::CHANNEL,
    responses(
        (status = 200, description = "Channel state"),
        (status = 404, description = "Channel not found", body = ErrorBody),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn channel(State(handle): State<Handle>, Path(id): Path<ChannelId>) -> Response {
    make_request_and_return_response!(channel::state(&handle, id))
}

#[utoipa::path(
    post,
    path = paths::CHANNEL_DEPOSIT,
    responses(
        (status = 200, description = "Submit a channel deposit"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn channel_deposit(
    State(handle): State<Handle>,
    Json(req): Json<ChannelDepositRequestBody>,
) -> Response {
    make_request_and_return_response!(channel::deposit(&handle, req))
}
