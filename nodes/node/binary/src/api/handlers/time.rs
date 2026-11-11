use axum::{extract::State, response::Response};
use lb_http_api_common::{TimeInfo, paths};

use crate::api::{
    calls::{Handle, time},
    errors::ErrorBody,
};

#[utoipa::path(
    get,
    path = paths::TIME_INFO,
    responses(
        (status = 200, description = "Query time service information", body = TimeInfo),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn time_info(State(handle): State<Handle>) -> Response {
    make_request_and_return_response!(time::info(&handle))
}
