use std::{
    fmt::{Debug, Display},
    time::Duration,
};

use async_trait::async_trait;
use backends::NetworkBackend;
use futures::StreamExt as _;
use lb_log_targets::network_service;
use lb_services_utils::wait_until_services_are_ready;
use lb_time_service::{EraEvent, TimeService, api::TimeServiceApi};
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        resources::ServiceResourcesHandle,
        state::{NoOperator, NoState},
    },
};

use crate::{config::NetworkConfig, message::BackendNetworkMsg};

pub mod backends;
pub mod config;
pub mod message;
mod metrics;

const LOG_TARGET: &str = network_service::ROOT;

pub struct NetworkService<Backend, TimeBackend, RuntimeServiceId>
where
    Backend: NetworkBackend<RuntimeServiceId> + 'static,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
}

impl<Backend, TimeBackend, RuntimeServiceId> ServiceData
    for NetworkService<Backend, TimeBackend, RuntimeServiceId>
where
    Backend: NetworkBackend<RuntimeServiceId> + 'static,
{
    type Settings = NetworkConfig<Backend::Settings>;
    type State = NoState<NetworkConfig<Backend::Settings>>;
    type StateOperator = NoOperator<Self::State>;
    type Message = BackendNetworkMsg<Backend, RuntimeServiceId>;
}

#[async_trait]
impl<Backend, TimeBackend, RuntimeServiceId> ServiceCore<RuntimeServiceId>
    for NetworkService<Backend, TimeBackend, RuntimeServiceId>
where
    Backend: NetworkBackend<RuntimeServiceId> + Send + 'static,
    TimeBackend: lb_time_service::backends::TimeBackend,
    RuntimeServiceId: AsServiceId<Self>
        + AsServiceId<TimeService<TimeBackend, RuntimeServiceId>>
        + Clone
        + Debug
        + Display
        + Send
        + Sync
        + 'static,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        _initial_state: Self::State,
    ) -> Result<Self, DynError> {
        Ok(Self {
            service_resources_handle,
        })
    }

    async fn run(mut self) -> Result<(), DynError> {
        let overwatch_handle = self.service_resources_handle.overwatch_handle.clone();

        // The backend starts in the era the clock is in, and follows the eras
        // the clock enters after it.
        wait_until_services_are_ready!(
            &overwatch_handle,
            Some(Duration::from_mins(1)),
            TimeService<TimeBackend, RuntimeServiceId>
        )
        .await?;

        let (current_era, mut era_events) =
            TimeServiceApi::<TimeService<TimeBackend, RuntimeServiceId>>::new(&overwatch_handle)
                .await?
                .subscribe_to_eras()
                .await?;

        let mut backend = Backend::new(
            self.service_resources_handle
                .settings_handle
                .notifier()
                .get_updated_settings()
                .backend,
            current_era,
            overwatch_handle,
        );

        let Self {
            service_resources_handle:
                ServiceResourcesHandle {
                    mut inbound_relay, ..
                },
        } = self;

        self.service_resources_handle.status_updater.notify_ready();
        tracing::info!(
            target: LOG_TARGET,
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        loop {
            tokio::select! {
                msg = inbound_relay.recv() => {
                    let Some(msg) = msg else {
                        break;
                    };
                    Self::handle_network_service_message(msg, &mut backend).await;
                }
                Some(era_event) = era_events.next() => match era_event {
                    EraEvent::NewEra { era, .. } => backend.rotate_era(era).await,
                    EraEvent::TransitionPeriodExpired(transition_end) => {
                        backend.complete_era_transition(transition_end.era()).await;
                    }
                },
            }
        }

        Ok(())
    }
}

impl<Backend, TimeBackend, RuntimeServiceId> NetworkService<Backend, TimeBackend, RuntimeServiceId>
where
    Backend: NetworkBackend<RuntimeServiceId> + Send + 'static,
{
    async fn handle_network_service_message(
        msg: BackendNetworkMsg<Backend, RuntimeServiceId>,
        backend: &mut Backend,
    ) {
        match msg {
            BackendNetworkMsg::<Backend, _>::Process(msg) => {
                // split sending in two steps to help the compiler understand we do not
                // need to hold an instance of &I (which is not send) across an await point
                let send = backend.process(msg);
                send.await;
            }
            BackendNetworkMsg::<Backend, _>::SubscribeToPubSub { sender } => sender
                .send(backend.subscribe_to_pubsub().await)
                .unwrap_or_else(|_| {
                    tracing::warn!(
                        target: LOG_TARGET,
                        "client hung up before a subscription handle could be established"
                    );
                }),
            BackendNetworkMsg::<Backend, _>::SubscribeToChainSync { sender } => sender
                .send(backend.subscribe_to_chainsync().await)
                .unwrap_or_else(|_| {
                    tracing::warn!(
                        target: LOG_TARGET,
                        "client hung up before a subscription handle could be established"
                    );
                }),
        }
    }
}
