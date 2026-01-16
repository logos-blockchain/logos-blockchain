#![allow(
    clippy::disallowed_script_idents,
    reason = "The crate `cfg_eval` contains Sinhala script identifiers. \
    Using the `expect` or `allow` macro on top of their usage does not remove the warning"
)]

use core::num::NonZero;
use std::{
    fmt::{Debug, Display, Formatter},
    pin::Pin,
};

use cryptarchia_engine::{Epoch, EpochConfig, Slot, time::SlotConfig};
use futures::{Stream, StreamExt as _};
use log::error;
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        state::{NoOperator, NoState},
    },
};
use tokio::sync::{broadcast, oneshot};
use tokio_stream::wrappers::BroadcastStream;

use crate::backends::TimeBackend;

pub mod backends;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SlotTick {
    pub epoch: Epoch,
    pub slot: Slot,
}

pub type EpochSlotTickStream = Pin<Box<dyn Stream<Item = SlotTick> + Send + Sync + Unpin>>;

pub enum TimeServiceMessage {
    Subscribe {
        sender: oneshot::Sender<EpochSlotTickStream>,
    },
    CurrentSlot {
        sender: oneshot::Sender<SlotTick>,
    },
}

impl Debug for TimeServiceMessage {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Subscribe { .. } => f.write_str("Subscribe"),
            Self::CurrentSlot { .. } => f.write_str("CurrentSlot"),
        }
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
pub struct TimeServiceSettings<BackendSettings> {
    /// Slot settings in order to compute proper slot times
    pub slot_config: SlotConfig,
    /// Epoch settings in order to compute proper epoch times
    pub epoch_config: EpochConfig,
    /// Base period length related to epochs, used to compute epochs as well
    pub base_period_length: NonZero<u64>,
    pub backend: BackendSettings,
}

pub struct TimeService<Backend, RuntimeServiceId>
where
    Backend: TimeBackend,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    backend: Backend,
}

impl<Backend, RuntimeServiceId> ServiceData for TimeService<Backend, RuntimeServiceId>
where
    Backend: TimeBackend,
{
    type Settings = TimeServiceSettings<Backend::Settings>;
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = TimeServiceMessage;
}

#[async_trait::async_trait]
impl<Backend, RuntimeServiceId> ServiceCore<RuntimeServiceId>
    for TimeService<Backend, RuntimeServiceId>
where
    Backend: TimeBackend + Send,
    Backend::Settings: Clone + Send + Sync,
    RuntimeServiceId: AsServiceId<Self> + Display + Send,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        _initial_state: Self::State,
    ) -> Result<Self, DynError> {
        let settings = service_resources_handle
            .settings_handle
            .notifier()
            .get_updated_settings();
        let backend = Backend::init(settings);
        Ok(Self {
            service_resources_handle,
            backend,
        })
    }

    async fn run(self) -> Result<(), DynError> {
        // 3 slots buffer should be enough
        const SLOTS_BUFFER: usize = 3;

        let Self {
            service_resources_handle,
            backend,
        } = self;
        let mut inbound_relay = service_resources_handle.inbound_relay;
        let (mut current_slot_tick, mut tick_stream) = backend.tick_stream();

        let (broadcast_sender, broadcast_receiver) = broadcast::channel(SLOTS_BUFFER);

        service_resources_handle.status_updater.notify_ready();
        tracing::info!(
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        loop {
            tokio::select! {
                Some(service_message) = inbound_relay.recv() => {
                    handle_service_message(service_message, &broadcast_receiver, &current_slot_tick);
                }
                Some(slot_tick) = tick_stream.next() => {
                    current_slot_tick = slot_tick;
                    if let Err(e) = broadcast_sender.send(slot_tick) {
                        error!("Error updating slot tick: {e}");
                    }
                }
            }
        }
    }
}

fn handle_service_message(
    message: TimeServiceMessage,
    broadcast_receiver: &broadcast::Receiver<SlotTick>,
    current_slot_tick: &SlotTick,
) {
    match message {
        TimeServiceMessage::Subscribe { sender } => {
            let channel_stream =
                BroadcastStream::new(broadcast_receiver.resubscribe()).filter_map(|result| {
                    Box::pin(async {
                        match result {
                            Ok(tick) => Some(tick),
                            Err(e) => {
                                // log lagging errors, services should always aim to be ready for
                                // next slot
                                error!("Lagging behind slot ticks: {e:?}");
                                None
                            }
                        }
                    })
                });
            let stream = Pin::new(Box::new(channel_stream));
            if sender.send(stream).is_err() {
                error!("Couldn't send back a Subscribe response");
            }
        }
        TimeServiceMessage::CurrentSlot { sender } => {
            if sender.send(*current_slot_tick).is_err() {
                error!("Couldn't send back a CurrentSlot response");
            }
        }
    }
}
