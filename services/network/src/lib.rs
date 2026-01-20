use std::fmt::{Debug, Display};

use async_trait::async_trait;
use backends::NetworkBackend;
use overwatch::{
    OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        state::{NoOperator, NoState},
    },
};

use crate::{config::NetworkConfig, message::BackendNetworkMsg};

pub mod backends;
pub mod config;
pub mod message;

pub struct NetworkService<Backend, RuntimeServiceId>
where
    Backend: NetworkBackend<RuntimeServiceId> + 'static,
{
    backend: Backend,
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
}

impl<Backend, RuntimeServiceId> ServiceData for NetworkService<Backend, RuntimeServiceId>
where
    Backend: NetworkBackend<RuntimeServiceId> + 'static,
{
    type Settings = NetworkConfig<Backend::Settings>;
    type State = NoState<NetworkConfig<Backend::Settings>>;
    type StateOperator = NoOperator<Self::State>;
    type Message = BackendNetworkMsg<Backend, RuntimeServiceId>;
}

#[async_trait]
impl<Backend, RuntimeServiceId> ServiceCore<RuntimeServiceId>
    for NetworkService<Backend, RuntimeServiceId>
where
    Backend: NetworkBackend<RuntimeServiceId> + Send + 'static,
    RuntimeServiceId: AsServiceId<Self> + Clone + Display + Send,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        _initial_state: Self::State,
    ) -> Result<Self, overwatch::DynError> {
        Ok(Self {
            backend: Backend::new(
                service_resources_handle
                    .settings_handle
                    .notifier()
                    .get_updated_settings()
                    .backend,
                service_resources_handle.overwatch_handle.clone(),
            ),
            service_resources_handle,
        })
    }

    async fn run(mut self) -> Result<(), overwatch::DynError> {
        let Self {
            service_resources_handle:
                OpaqueServiceResourcesHandle::<Self, RuntimeServiceId> {
                    mut inbound_relay, ..
                },
            mut backend,
        } = self;

        self.service_resources_handle.status_updater.notify_ready();
        tracing::info!(
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        while let Some(msg) = inbound_relay.recv().await {
            Self::handle_network_service_message(msg, &mut backend).await;
        }

        Ok(())
    }
}

impl<Backend, RuntimeServiceId> NetworkService<Backend, RuntimeServiceId>
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
                        "client hung up before a subscription handle could be established"
                    );
                }),
            BackendNetworkMsg::<Backend, _>::SubscribeToChainSync { sender } => sender
                .send(backend.subscribe_to_chainsync().await)
                .unwrap_or_else(|_| {
                    tracing::warn!(
                        "client hung up before a subscription handle could be established"
                    );
                }),
        }
    }
}
