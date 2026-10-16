use std::{
    fmt::{Debug, Display, Formatter},
    pin::Pin,
};

use futures::{Stream, StreamExt as _};
use lb_log_targets::time as log_targets_time;
use lb_time::{Epoch, Era, Slot, era::EraTimingSchedule};
use log::error;
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        state::{NoOperator, NoState},
    },
};
use time::OffsetDateTime;
use tokio::sync::{oneshot, watch};
use tokio_stream::wrappers::WatchStream;

use crate::backends::TimeBackend;

pub mod api;
pub mod backends;
mod metrics;

const LOG_TARGET: &str = log_targets_time::ROOT;

/// Service-owned struct for time information
/// This is the internal representation used by the time service
/// and is mapped to the API response struct by the API layer
#[derive(Clone, Debug)]
pub struct TimeServiceInfo {
    pub current_slot: Slot,
    pub current_epoch: Epoch,
    pub era_schedules: EraTimingSchedule,
}

impl TimeServiceInfo {
    #[must_use]
    pub const fn genesis_time(&self) -> OffsetDateTime {
        self.era_schedules.genesis().start_time
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SlotTick {
    pub era: Era,
    pub epoch: Epoch,
    pub slot: Slot,
}

pub type EpochSlotTickStream = Pin<Box<dyn Stream<Item = SlotTick> + Send + Sync + Unpin>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EraTick {
    pub era: Era,
    pub slot: Slot,
}

pub type EraTickStream = Pin<Box<dyn Stream<Item = EraTick> + Send + Sync + Unpin>>;

pub enum TimeServiceMessage {
    Info {
        sender: oneshot::Sender<TimeServiceInfo>,
    },
    SubscribeToEpochSlots {
        sender: oneshot::Sender<EpochSlotTickStream>,
    },
    SubscribeToEras {
        sender: oneshot::Sender<EraTickStream>,
    },
    CurrentSlot {
        sender: oneshot::Sender<SlotTick>,
    },
}

impl Debug for TimeServiceMessage {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Info { .. } => f.write_str("Info"),
            Self::SubscribeToEpochSlots { .. } => f.write_str("SubscribeToEpochSlots"),
            Self::SubscribeToEras { .. } => f.write_str("SubscribeToEras"),
            Self::CurrentSlot { .. } => f.write_str("CurrentSlot"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct TimeServiceSettings<BackendSettings> {
    pub backend: BackendSettings,
    pub era_schedule: EraTimingSchedule,
}

pub struct TimeService<Backend, RuntimeServiceId>
where
    Backend: TimeBackend,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    backend: Backend,
    era_schedule: EraTimingSchedule,
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
        let TimeServiceSettings {
            backend: backend_settings,
            era_schedule,
        } = service_resources_handle
            .settings_handle
            .notifier()
            .get_updated_settings();
        let backend = Backend::init(backend_settings, era_schedule.clone());
        Ok(Self {
            service_resources_handle,
            backend,
            era_schedule,
        })
    }

    async fn run(self) -> Result<(), DynError> {
        let Self {
            service_resources_handle,
            backend,
            era_schedule,
        } = self;
        let mut inbound_relay = service_resources_handle.inbound_relay;
        let (mut current_slot_tick, mut tick_stream) = backend.tick_stream();

        let (watch_sender, watch_receiver) = watch::channel(current_slot_tick);

        let mut current_era = current_slot_tick.era;
        let (era_sender, era_receiver) = watch::channel(EraTick {
            era: current_era,
            slot: current_slot_tick.slot,
        });

        service_resources_handle.status_updater.notify_ready();
        tracing::info!(
            target: LOG_TARGET,
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        loop {
            tokio::select! {
                Some(service_message) = inbound_relay.recv() => {
                    handle_service_message(
                        service_message,
                        &watch_receiver,
                        &era_receiver,
                        &current_slot_tick,
                        &era_schedule,
                    );
                }
                Some(slot_tick) = tick_stream.next() => {
                    current_slot_tick = slot_tick;
                    metrics::time_current_slot(u64::from(slot_tick.slot));
                    metrics::time_current_epoch(u32::from(slot_tick.epoch));

                    if let Err(e) = watch_sender.send(slot_tick) {
                        error!(target: LOG_TARGET, "Error updating slot tick: {e}");
                        metrics::time_broadcast_errors();
                    }

                    if slot_tick.era > current_era {
                        current_era = slot_tick.era;
                        if let Err(e) = era_sender.send(EraTick {
                            era: slot_tick.era,
                            slot: slot_tick.slot,
                        }) {
                            error!(target: LOG_TARGET, "Error updating era tick: {e}");
                            metrics::time_broadcast_errors();
                        }
                    }
                }
            }
        }
    }
}

fn handle_service_message(
    message: TimeServiceMessage,
    watch_receiver: &watch::Receiver<SlotTick>,
    era_tick_receiver: &watch::Receiver<EraTick>,
    current_slot_tick: &SlotTick,
    era_schedule: &EraTimingSchedule,
) {
    match message {
        TimeServiceMessage::Info { sender } => {
            drop(sender.send(TimeServiceInfo {
                current_slot: current_slot_tick.slot,
                current_epoch: current_slot_tick.epoch,
                era_schedules: era_schedule.clone(),
            }));
        }
        TimeServiceMessage::SubscribeToEpochSlots { sender } => {
            let stream = Pin::new(Box::new(WatchStream::from_changes(watch_receiver.clone())));
            if sender.send(stream).is_err() {
                error!(target: LOG_TARGET, "Couldn't send back a SubscribeToEpochSlots response");
            }
        }
        TimeServiceMessage::SubscribeToEras { sender } => {
            let stream = Pin::new(Box::new(WatchStream::new(era_tick_receiver.clone())));
            if sender.send(stream).is_err() {
                error!(target: LOG_TARGET, "Couldn't send back a SubscribeToEras response");
            }
        }
        TimeServiceMessage::CurrentSlot { sender } => {
            if sender.send(*current_slot_tick).is_err() {
                error!(target: LOG_TARGET, "Couldn't send back a CurrentSlot response");
            }
        }
    }
}
