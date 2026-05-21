use std::fmt::{Debug, Display};

use axum::{Json, extract::State, response::Response};
use lb_api_service::http::DynError;
use lb_tracing::filter::envfilter::EnvFilterConfig;
use lb_tracing_service::TracingMessage;
use overwatch::{overwatch::handle::OverwatchHandle, services::AsServiceId};

use crate::{TracingService, make_request_and_return_response};

const FILTER_RELOAD_LOG_TARGET: &str = "admin_tracing_filter_reload";

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

    tracing::debug!(target: FILTER_RELOAD_LOG_TARGET, "Tracing filter reloaded");

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
        .map_err(|(err, _)| err)?;

    reply_rx
        .await
        .map_err(|err| Box::new(err) as DynError)?
        .map_err(Into::into)
}
