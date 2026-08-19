use core::future::pending;

use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{
        ServiceCore, ServiceData,
        state::{NoOperator, NoState},
    },
};

/// A stand-in for the mempool service a [`PayloadDispatcher`] hands
/// transactions to.
///
/// A test dispatcher holds the relay but never sends on it, so the service only
/// has to exist: it registers under a runtime service ID and then parks.
///
/// [`PayloadDispatcher`]: crate::core::dispatcher::PayloadDispatcher
pub struct TestMempoolService<RuntimeServiceId> {
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
}

impl<RuntimeServiceId> ServiceData for TestMempoolService<RuntimeServiceId> {
    type Settings = ();
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = ();
}

#[async_trait::async_trait]
impl<RuntimeServiceId> ServiceCore<RuntimeServiceId> for TestMempoolService<RuntimeServiceId>
where
    RuntimeServiceId: Send,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        _initial_state: Self::State,
    ) -> Result<Self, DynError> {
        Ok(Self {
            service_resources_handle,
        })
    }

    async fn run(self) -> Result<(), DynError> {
        self.service_resources_handle.status_updater.notify_ready();
        pending().await
    }
}
