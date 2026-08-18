use std::marker::PhantomData;

use overwatch::services::{ServiceData, relay::OutboundRelay};
use thiserror::Error;
use tokio::sync::oneshot;

use crate::{EpochSlotTickStream, SlotTick, TimeServiceInfo, TimeServiceMessage};

/// Marker trait for the time service, used to parametrize [`TimeServiceApi`]
/// over the concrete time service type while pinning its message type.
pub trait TimeServiceData: ServiceData<Message = TimeServiceMessage> + Send + 'static {}
impl<T> TimeServiceData for T where T: ServiceData<Message = TimeServiceMessage> + Send + 'static {}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("Failed to establish connection to time-service: {0}")]
    CommsFailure(String),
    #[error("Unexpected Error: {0}")]
    Unexpected(String),
}

/// Typed wrapper over the time service relay, exposing the time queries as
/// async methods instead of raw [`TimeServiceMessage`]s.
pub struct TimeServiceApi<Time, RuntimeServiceId>
where
    Time: TimeServiceData,
{
    relay: OutboundRelay<Time::Message>,
    _id: PhantomData<RuntimeServiceId>,
}

impl<Time, RuntimeServiceId> Clone for TimeServiceApi<Time, RuntimeServiceId>
where
    Time: TimeServiceData,
{
    fn clone(&self) -> Self {
        Self {
            relay: self.relay.clone(),
            _id: PhantomData,
        }
    }
}

impl<Time, RuntimeServiceId> TimeServiceApi<Time, RuntimeServiceId>
where
    Time: TimeServiceData,
    RuntimeServiceId: Sync,
{
    #[must_use]
    pub const fn new(relay: OutboundRelay<Time::Message>) -> Self {
        Self {
            relay,
            _id: PhantomData,
        }
    }

    /// Get the current time information (slot duration, genesis time, current
    /// slot and epoch).
    pub async fn info(&self) -> Result<TimeServiceInfo, ApiError> {
        let (sender, receiver) = oneshot::channel();

        self.relay
            .send(TimeServiceMessage::Info { sender })
            .await
            .map_err(|(relay_error, _)| {
                ApiError::CommsFailure(format!("{relay_error} while sending Info"))
            })?;

        receiver
            .await
            .map_err(|relay_error| {
                ApiError::CommsFailure(format!("{relay_error} while receiving Info"))
            })?
            .map_err(ApiError::Unexpected)
    }

    /// Get the current slot tick.
    pub async fn current_slot(&self) -> Result<SlotTick, ApiError> {
        let (sender, receiver) = oneshot::channel();

        self.relay
            .send(TimeServiceMessage::CurrentSlot { sender })
            .await
            .map_err(|(relay_error, _)| {
                ApiError::CommsFailure(format!("{relay_error} while sending CurrentSlot"))
            })?;

        receiver.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while receiving CurrentSlot"))
        })
    }

    /// Subscribe to the stream of slot ticks.
    pub async fn subscribe(&self) -> Result<EpochSlotTickStream, ApiError> {
        let (sender, receiver) = oneshot::channel();

        self.relay
            .send(TimeServiceMessage::Subscribe { sender })
            .await
            .map_err(|(relay_error, _)| {
                ApiError::CommsFailure(format!("{relay_error} while sending Subscribe"))
            })?;

        receiver.await.map_err(|relay_error| {
            ApiError::CommsFailure(format!("{relay_error} while receiving Subscribe"))
        })
    }
}
