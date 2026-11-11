use axum::{
    Extension, Json,
    extract::State,
    response::{IntoResponse as _, Response},
};
use lb_core::mantle::transactions::genesis_tx::ChainId;
use lb_http_api_common::{bodies::chain::ChainIdResponseBody, paths};
use lb_log_targets::node as targets;
use lb_tracing::filter::envfilter::EnvFilterConfig;
use lb_version::BuildVersionInfo;

use crate::api::calls::{self, Handle, node};

#[utoipa::path(
    get,
    path = paths::NODE_VERSION,
    responses(
        (status = 200, description = "Version and build provenance of the running node", body = BuildVersionInfo),
    )
)]
pub async fn version() -> Response {
    Json(lb_version::build_version_info()).into_response()
}

/// The chain ID is fixed by the deployment the node was built with, so it is
/// handed to the API backend in its settings and served straight from the
/// request extensions. There is no failure path.
#[utoipa::path(
    get,
    path = paths::CHAIN_ID,
    responses(
        (status = 200, description = "The chain this node runs on", body = String),
    )
)]
pub async fn chain_id(Extension(chain_id): Extension<ChainId>) -> Response {
    Json(ChainIdResponseBody { chain_id }).into_response()
}

const LOG_TARGET: &str = targets::api::TRACING;

#[utoipa::path(
    put,
    path = paths::admin::TRACING_FILTER,
    responses(
        (status = 200, description = "Tracing filter reloaded"),
        (status = 500, description = "Internal server error", body = crate::api::errors::ErrorBody),
    )
)]
pub async fn reload_tracing_filter(
    State(handle): State<Handle>,
    Json(filter): Json<EnvFilterConfig>,
) -> Response {
    make_request_and_return_response!(async {
        node::reload_tracing_filter(&handle, filter).await?;
        tracing::debug!(target: LOG_TARGET, "Tracing filter reloaded");
        Ok::<_, calls::Error>(())
    })
}
