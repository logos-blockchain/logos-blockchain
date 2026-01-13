use std::fmt::{Debug, Display};

use axum::response::{IntoResponse as _, Response};
use http::StatusCode;
use overwatch::{
    overwatch::OverwatchHandle,
    services::{AsServiceId, ServiceData, relay::OutboundRelay},
};

pub async fn get_relay_or_500<Service, RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
) -> Result<OutboundRelay<<Service as ServiceData>::Message>, Response>
where
    Service: ServiceData,
    Service::Message: 'static,
    RuntimeServiceId: Debug + Sync + Display + AsServiceId<Service>,
{
    handle
        .relay()
        .await
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response())
}
