use core::num::NonZero;
use std::{
    fmt::{Debug, Display, Formatter},
    pin::Pin,
};

use futures::{Stream, StreamExt as _};
use lb_cryptarchia_engine::{Epoch, EpochConfig, Slot, time::SlotConfig};
use lb_log_targets::time as log_targets_time;
use log::error;
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        state::{NoOperator, NoState},
    },
};
use tokio::sync::{oneshot, watch};
use tokio_stream::wrappers::WatchStream;

use crate::backends::TimeBackend;

pub mod backends;
mod metrics;

const LOG_TARGET: &str = log_targets_time::ROOT;

/// Service-owned struct for time information
/// This is the internal representation used by the time service
/// and is mapped to the API response struct by the API layer
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TimeServiceInfo {
    pub slot_duration_ms: u64,
    pub genesis_time_unix_ms: i64,
    pub current_slot: Slot,
    pub current_epoch: Epoch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SlotTick {
    pub epoch: Epoch,
    pub slot: Slot,
}

pub type EpochSlotTickStream = Pin<Box<dyn Stream<Item = SlotTick> + Send + Sync + Unpin>>;

pub enum TimeServiceMessage {
    Info {
        sender: oneshot::Sender<Result<TimeServiceInfo, String>>,
    },
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
            Self::Info { .. } => f.write_str("Info"),
            Self::Subscribe { .. } => f.write_str("Subscribe"),
            Self::CurrentSlot { .. } => f.write_str("CurrentSlot"),
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
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
    settings: TimeServiceSettings<Backend::Settings>,
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
        let backend = Backend::init(settings.clone());
        Ok(Self {
            service_resources_handle,
            backend,
            settings,
        })
    }

    async fn run(self) -> Result<(), DynError> {
        let Self {
            service_resources_handle,
            backend,
            settings,
        } = self;
        let mut inbound_relay = service_resources_handle.inbound_relay;
        let (mut current_slot_tick, mut tick_stream) = backend.tick_stream();

        let (watch_sender, watch_receiver) = watch::channel(current_slot_tick);

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
                        &current_slot_tick,
                        &settings,
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
                }
            }
        }
    }
}

fn handle_service_message<BackendSettings>(
    message: TimeServiceMessage,
    watch_receiver: &watch::Receiver<SlotTick>,
    current_slot_tick: &SlotTick,
    settings: &TimeServiceSettings<BackendSettings>,
) {
    match message {
        TimeServiceMessage::Info { sender } => {
            let Ok(slot_duration_ms) =
                u64::try_from(settings.slot_config.slot_duration.as_millis())
            else {
                drop(sender.send(Err(
                    "slot duration exceeds u64::MAX milliseconds".to_owned(),
                )));
                return;
            };
            let Ok(genesis_time_unix_ms) = i64::try_from(
                settings
                    .slot_config
                    .genesis_time
                    .unix_timestamp_nanos()
                    .div_euclid(1_000_000),
            ) else {
                drop(sender.send(Err("genesis time exceeds i64::MAX milliseconds".to_owned())));
                return;
            };
            drop(sender.send(Ok(TimeServiceInfo {
                slot_duration_ms,
                genesis_time_unix_ms,
                current_slot: current_slot_tick.slot,
                current_epoch: current_slot_tick.epoch,
            })));
        }
        TimeServiceMessage::Subscribe { sender } => {
            let stream = Pin::new(Box::new(WatchStream::from_changes(watch_receiver.clone())));
            if sender.send(stream).is_err() {
                error!(target: LOG_TARGET, "Couldn't send back a Subscribe response");
            }
        }
        TimeServiceMessage::CurrentSlot { sender } => {
            if sender.send(*current_slot_tick).is_err() {
                error!(target: LOG_TARGET, "Couldn't send back a CurrentSlot response");
            }
        }
    }
}
