use std::{
    fmt::{Debug, Display, Formatter},
    pin::Pin,
    time::Duration,
};

use futures::{Stream, StreamExt as _, future, stream};
use lb_log_targets::time as log_targets_time;
use lb_time::{
    Epoch, Era, Slot,
    era::{EraSchedule, EraTiming, EraTimingSchedule, SlotDuration},
};
use log::error;
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        state::{NoOperator, NoState},
    },
};
use time::OffsetDateTime;
use tokio::sync::{broadcast, oneshot, watch};
use tokio_stream::wrappers::{BroadcastStream, WatchStream};

use crate::{backends::TimeBackend, era_events::EraTransitionsTracker};

pub mod api;
pub mod backends;
mod era_events;
mod metrics;

const LOG_TARGET: &str = log_targets_time::ROOT;

/// This will always be at most two events for an era: a new era started and the
/// old era transition period elapsed.
const ERA_EVENTS_CAPACITY: usize = 2;

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

/// A change the clock brings to the eras.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EraEvent {
    NewEra { era: Era, first_slot: Slot },
    TransitionPeriodExpired(EraTransitionEnd),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EraTransitionEnd {
    era: Era,
}

impl EraTransitionEnd {
    /// The era whose transition period ended.
    #[must_use]
    pub const fn era(&self) -> Era {
        self.era
    }
}

pub type EraEventStream = Pin<Box<dyn Stream<Item = EraEvent> + Send + Sync + Unpin>>;
pub type EraTransitionEndStream =
    Pin<Box<dyn Stream<Item = EraTransitionEnd> + Send + Sync + Unpin>>;

pub enum TimeServiceMessage {
    Info {
        sender: oneshot::Sender<TimeServiceInfo>,
    },
    SubscribeToEpochSlots {
        sender: oneshot::Sender<EpochSlotTickStream>,
    },
    /// The era in force, as a first new era, and the era events after it.
    SubscribeToEras {
        sender: oneshot::Sender<EraEventStream>,
    },
    /// The ends of the transition periods to come.
    SubscribeToEraTransitionEnds {
        sender: oneshot::Sender<EraTransitionEndStream>,
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
            Self::SubscribeToEraTransitionEnds { .. } => {
                f.write_str("SubscribeToEraTransitionEnds")
            }
            Self::CurrentSlot { .. } => f.write_str("CurrentSlot"),
        }
    }
}

/// An era as the time service follows it: how its slots and epochs are laid
/// out, and for how many of its slots the network keeps serving its protocols
/// once the next era starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeServiceEraSettings {
    pub timing: EraTiming,
    pub transition_slots: u64,
}

impl SlotDuration for TimeServiceEraSettings {
    fn slot_duration(&self) -> Duration {
        self.timing.slot_duration
    }
}

#[derive(Clone, Debug)]
pub struct TimeServiceSettings<BackendSettings> {
    pub backend: BackendSettings,
    pub era_schedule: EraSchedule<TimeServiceEraSettings>,
}

pub struct TimeService<Backend, RuntimeServiceId>
where
    Backend: TimeBackend,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    backend: Backend,
    era_schedule: EraSchedule<TimeServiceEraSettings>,
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
        let backend = Backend::init(
            backend_settings,
            era_schedule.map(|era| era.parameters.timing),
        );
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

        let mut era_transitions_tracker =
            EraTransitionsTracker::new(current_slot_tick, &era_schedule);
        let (era_events_sender, _) = broadcast::channel(ERA_EVENTS_CAPACITY);

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
                        &era_events_sender,
                        &era_transitions_tracker,
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

                    if let Some(era_event) = era_transitions_tracker.advance(slot_tick) {
                        let _ = era_events_sender.send(era_event);
                    }
                }
            }
        }
    }
}

fn handle_service_message(
    message: TimeServiceMessage,
    watch_receiver: &watch::Receiver<SlotTick>,
    era_events: &broadcast::Sender<EraEvent>,
    era_transitions_tracker: &EraTransitionsTracker<'_>,
    current_slot_tick: &SlotTick,
    era_schedule: &EraSchedule<TimeServiceEraSettings>,
) {
    match message {
        TimeServiceMessage::Info { sender } => {
            drop(sender.send(TimeServiceInfo {
                current_slot: current_slot_tick.slot,
                current_epoch: current_slot_tick.epoch,
                era_schedules: era_schedule.map(|era| era.parameters.timing),
            }));
        }
        TimeServiceMessage::SubscribeToEpochSlots { sender } => {
            let stream = Pin::new(Box::new(WatchStream::from_changes(watch_receiver.clone())));
            if sender.send(stream).is_err() {
                error!(target: LOG_TARGET, "Couldn't send back a SubscribeToEpochSlots response");
            }
        }
        TimeServiceMessage::SubscribeToEras { sender } => {
            let era_in_force = era_transitions_tracker.era_in_force();
            let stream = Pin::new(Box::new(
                stream::once(future::ready(EraEvent::NewEra {
                    era: era_in_force.era,
                    first_slot: era_in_force.first_slot,
                }))
                .chain(era_event_stream(era_events.subscribe())),
            ));
            if sender.send(stream).is_err() {
                error!(target: LOG_TARGET, "Couldn't send back a SubscribeToEras response");
            }
        }
        TimeServiceMessage::SubscribeToEraTransitionEnds { sender } => {
            let stream = Pin::new(Box::new(
                era_event_stream(era_events.subscribe()).filter_map(|event| {
                    future::ready(match event {
                        EraEvent::TransitionPeriodExpired(end) => Some(end),
                        EraEvent::NewEra { .. } => None,
                    })
                }),
            ));
            if sender.send(stream).is_err() {
                error!(
                    target: LOG_TARGET,
                    "Couldn't send back a SubscribeToEraTransitionEnds response"
                );
            }
        }
        TimeServiceMessage::CurrentSlot { sender } => {
            if sender.send(*current_slot_tick).is_err() {
                error!(target: LOG_TARGET, "Couldn't send back a CurrentSlot response");
            }
        }
    }
}

/// The era events `receiver` gets, the ones it misses by falling behind
/// reported in the log.
fn era_event_stream(receiver: broadcast::Receiver<EraEvent>) -> EraEventStream {
    Pin::new(Box::new(BroadcastStream::new(receiver).filter_map(
        |event| {
            future::ready(
                event
                    .inspect_err(|error| {
                        error!(target: LOG_TARGET, "An era event subscriber fell behind: {error}");
                    })
                    .ok(),
            )
        },
    )))
}
