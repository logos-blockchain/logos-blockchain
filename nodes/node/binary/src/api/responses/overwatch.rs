use std::fmt::{Debug, Display};

use overwatch::{
    overwatch::OverwatchHandle,
    services::{AsServiceId, ServiceData, relay::OutboundRelay},
};

use crate::api::errors::ApiError;

pub async fn get_relay<Service, RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
) -> Result<OutboundRelay<<Service as ServiceData>::Message>, ApiError>
where
    Service: ServiceData,
    Service::Message: 'static,
    RuntimeServiceId: Debug + Sync + Display + AsServiceId<Service>,
{
    handle.relay().await.map_err(ApiError::internal)
}
