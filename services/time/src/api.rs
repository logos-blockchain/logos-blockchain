use std::fmt::{Debug, Display};

use futures::StreamExt as _;
use lb_time::Era;
use overwatch::{
    DynError,
    overwatch::{OverwatchHandle, errors::Error as OverwatchError},
    services::{AsServiceId, ServiceData, relay::OutboundRelay},
};
use tokio::sync::oneshot;

use crate::{
    EpochSlotTickStream, EraEvent, EraEventStream, EraTransitionEndStream, SlotTick,
    TimeServiceMessage,
};

/// Marker trait for the time service, used to parametrize [`TimeServiceApi`]
/// over the concrete service type while pinning its message type.
pub trait TimeServiceData: ServiceData<Message = TimeServiceMessage> {}

impl<T> TimeServiceData for T where T: ServiceData<Message = TimeServiceMessage> {}

/// Typed wrapper over the time service relay, exposing its operations as async
/// methods instead of raw [`TimeServiceMessage`]s.
#[derive(Clone)]
pub struct TimeServiceApi<TimeService>
where
    TimeService: TimeServiceData,
{
    relay: OutboundRelay<TimeService::Message>,
}

impl<TimeService> TimeServiceApi<TimeService>
where
    TimeService: TimeServiceData,
{
    /// Connect to the time service through the Overwatch handle.
    pub async fn new<RuntimeServiceId>(
        overwatch_handle: &OverwatchHandle<RuntimeServiceId>,
    ) -> Result<Self, OverwatchError>
    where
        RuntimeServiceId: AsServiceId<TimeService> + Debug + Display + Sync,
    {
        let relay = overwatch_handle.relay::<TimeService>().await?;
        Ok(Self { relay })
    }

    /// The era the clock is in, and a stream of the era events after it: the
    /// eras it enters, and the transition periods that expire.
    pub async fn subscribe_to_eras(&self) -> Result<(Era, EraEventStream), DynError> {
        let mut era_events = self
            .request(|sender| TimeServiceMessage::SubscribeToEras { sender })
            .await?;
        // The service opens the stream with the era in force.
        let Some(EraEvent::NewEra { era, .. }) = era_events.next().await else {
            panic!("The era event stream did not open with the era in force.")
        };
        Ok((era, era_events))
    }

    /// A stream of the transition periods that expire from now on.
    pub async fn subscribe_to_era_transition_ends(
        &self,
    ) -> Result<EraTransitionEndStream, DynError> {
        self.request(|sender| TimeServiceMessage::SubscribeToEraTransitionEnds { sender })
            .await
    }

    /// The slot the clock is in, and a stream of the slots it ticks after it.
    ///
    /// The stream is opened first, so no tick is lost in between: its first
    /// tick can be the returned one.
    pub async fn subscribe_to_epoch_slots(
        &self,
    ) -> Result<(SlotTick, EpochSlotTickStream), DynError> {
        let slot_ticks = self
            .request(|sender| TimeServiceMessage::SubscribeToEpochSlots { sender })
            .await?;
        let current_slot_tick = self
            .request(|sender| TimeServiceMessage::CurrentSlot { sender })
            .await?;
        Ok((current_slot_tick, slot_ticks))
    }

    async fn request<Reply>(
        &self,
        message: impl FnOnce(oneshot::Sender<Reply>) -> TimeServiceMessage,
    ) -> Result<Reply, DynError> {
        let (sender, receiver) = oneshot::channel();
        self.relay.send(message(sender)).await?;
        Ok(receiver.await?)
    }
}
