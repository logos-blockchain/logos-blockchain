use axum::{extract::State, response::Response};
use lb_api_common::{bodies::leader::LeaderClaimResponseBody, paths};

use crate::api::{
    calls::{Handle, leader},
    errors::ErrorBody,
};

#[utoipa::path(
    post,
    path = paths::LEADER_CLAIM,
    responses(
        (status = 200, description = "Leader claim transaction submitted", body = LeaderClaimResponseBody),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn leader_claim(State(handle): State<Handle>) -> Response {
    make_request_and_return_response!(leader::claim(&handle))
}
