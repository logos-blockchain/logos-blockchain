use std::fmt::Display;

use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    overwatch::handle::OverwatchHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        state::{NoOperator, NoState},
    },
};

pub mod http;

/// A simple abstraction so that we can easily
/// change the underlying http server
#[async_trait::async_trait]
pub trait Backend<RuntimeServiceId> {
    type Error: std::error::Error + Send + Sync + 'static;
    type Settings: Clone + Send + Sync + 'static;

    async fn new(settings: Self::Settings) -> Result<Self, Self::Error>
    where
        Self: Sized;

    async fn serve(self, handle: OverwatchHandle<RuntimeServiceId>) -> Result<(), Self::Error>;
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ApiServiceSettings<S> {
    pub backend_settings: S,
}

pub struct ApiService<B: Backend<RuntimeServiceId>, RuntimeServiceId> {
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    settings: ApiServiceSettings<B::Settings>,
}

impl<B: Backend<RuntimeServiceId>, RuntimeServiceId> ServiceData
    for ApiService<B, RuntimeServiceId>
{
    type Settings = ApiServiceSettings<B::Settings>;
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = ();
}

#[async_trait::async_trait]
impl<B, RuntimeServiceId> ServiceCore<RuntimeServiceId> for ApiService<B, RuntimeServiceId>
where
    B: Backend<RuntimeServiceId> + Send + Sync + 'static,
    RuntimeServiceId: AsServiceId<Self> + Display + Send + Clone,
{
    /// Initialize the service with the given state
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        _initial_state: Self::State,
    ) -> Result<Self, DynError> {
        let settings = service_resources_handle
            .settings_handle
            .notifier()
            .get_updated_settings();

        Ok(Self {
            service_resources_handle,
            settings,
        })
    }

    /// Service main loop
    async fn run(self) -> Result<(), DynError> {
        let endpoint = B::new(self.settings.backend_settings).await?;

        self.service_resources_handle.status_updater.notify_ready();
        tracing::info!(
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );

        endpoint
            .serve(self.service_resources_handle.overwatch_handle)
            .await?;

        Ok(())
    }
}
