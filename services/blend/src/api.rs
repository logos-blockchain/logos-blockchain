use std::marker::PhantomData;

use overwatch::services::{ServiceData, relay::OutboundRelay};
use thiserror::Error;

use crate::{
    ServiceComponents,
    message::{BlendPayload, ProxyServiceMessage, ServiceMessage},
};

/// Marker trait for the top-level blend service, used to parametrize
/// [`BlendServiceApi`] over the concrete blend service type while pinning its
/// message type.
pub trait BlendServiceData:
    ServiceData<Message = ProxyServiceMessage<ServiceMessage<<Self as ServiceComponents>::NodeId>>>
    + ServiceComponents
    + Send
    + 'static
{
}
impl<T> BlendServiceData for T where
    T: ServiceData<Message = ProxyServiceMessage<ServiceMessage<<T as ServiceComponents>::NodeId>>>
        + ServiceComponents
        + Send
        + 'static
{
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("Failed to establish connection to blend-service: {0}")]
    CommsFailure(String),
}

/// Typed wrapper over the blend service relay, exposing the blend queries and
/// the payload-publishing entry point as async methods instead of raw
/// [`ProxyServiceMessage`]s.
pub struct BlendServiceApi<Blend, RuntimeServiceId>
where
    Blend: BlendServiceData,
{
    relay: OutboundRelay<Blend::Message>,
    _id: PhantomData<RuntimeServiceId>,
}

impl<Blend, RuntimeServiceId> Clone for BlendServiceApi<Blend, RuntimeServiceId>
where
    Blend: BlendServiceData,
{
    fn clone(&self) -> Self {
        Self {
            relay: self.relay.clone(),
            _id: PhantomData,
        }
    }
}

impl<Blend, RuntimeServiceId> BlendServiceApi<Blend, RuntimeServiceId>
where
    Blend: BlendServiceData,
    Blend::NodeId: Send,
    RuntimeServiceId: Sync,
{
    #[must_use]
    pub const fn new(relay: OutboundRelay<Blend::Message>) -> Self {
        Self {
            relay,
            _id: PhantomData,
        }
    }

    /// Publish a payload to the blend network. The exit node hands it over to
    /// whichever local service owns that kind of payload. Fire-and-forget.
    pub async fn publish(&self, payload: BlendPayload) -> Result<(), ApiError> {
        self.relay
            .send(ServiceMessage::Blend(payload).into())
            .await
            .map_err(|(relay_error, _)| {
                ApiError::CommsFailure(format!("{relay_error} while sending Blend"))
            })
    }
}
