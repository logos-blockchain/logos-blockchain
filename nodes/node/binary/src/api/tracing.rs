use std::fmt::{Debug, Display};

use axum::{Json, extract::State, response::Response};
use lb_api_service::http::DynError;
use lb_http_api_common::paths;
use lb_log_targets::node;
use lb_tracing::filter::envfilter::EnvFilterConfig;
use lb_tracing_service::TracingMessage;
use overwatch::{overwatch::handle::OverwatchHandle, services::AsServiceId};

use crate::{TracingService, make_request_and_return_response};

const LOG_TARGET: &str = node::api::TRACING;

#[utoipa::path(
    put,
    path = paths::admin::TRACING_FILTER,
    tag = "Node",
    summary = "Replace the log filter",
    description = "Operator endpoint. Replaces the per-target tracing levels of the running node.",
    request_body = crate::api::openapi::schema::EnvFilterConfig,
    responses(
        (status = 200, description = "Tracing filter reloaded", body = crate::api::openapi::schema::Null),
        (status = 500, description = "Internal server error", body = crate::api::errors::ErrorBody),
    )
)]
pub async fn reload_tracing_filter<RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(filter): Json<EnvFilterConfig>,
) -> Response
where
    RuntimeServiceId: Debug + Send + Sync + Display + 'static + AsServiceId<TracingService>,
{
    make_request_and_return_response!(reload_filter(handle, filter))
}

async fn reload_filter<RuntimeServiceId>(
    handle: OverwatchHandle<RuntimeServiceId>,
    filter: EnvFilterConfig,
) -> Result<(), DynError>
where
    RuntimeServiceId: Debug + Send + Sync + Display + 'static + AsServiceId<TracingService>,
{
    request_filter_reload(handle, filter).await?;

    tracing::debug!(target: LOG_TARGET, "Tracing filter reloaded");

    Ok(())
}

async fn request_filter_reload<RuntimeServiceId>(
    handle: OverwatchHandle<RuntimeServiceId>,
    filter: EnvFilterConfig,
) -> Result<(), DynError>
where
    RuntimeServiceId: Debug + Send + Sync + Display + 'static + AsServiceId<TracingService>,
{
    let relay = handle.relay::<TracingService>().await?;
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();

    relay
        .send(TracingMessage::ReloadFilter {
            filter,
            reply_channel: reply_tx,
        })
        .await
        .map_err(|error| DynError::from(error.to_string()))?;

    reply_rx
        .await
        .map_err(|err| Box::new(err) as DynError)?
        .map_err(Into::into)
}
