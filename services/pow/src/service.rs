use core::fmt::{Debug, Display};
use std::marker::PhantomData;

use lb_chain_service::api::{CryptarchiaServiceApi, CryptarchiaServiceData};
use lb_core::mantle::ops::pow::ClaimPowRewardOp;
use lb_time_service::{TimeService, TimeServiceMessage, backends::TimeBackend as TimeBackendTrait};
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{AsServiceId, ServiceCore, ServiceData, relay::OutboundRelay},
};

#[derive(thiserror::Error, Debug)]
pub enum PoWServiceError {}
pub enum PoWSerciveMessage {}

pub struct PoWServiceState<Tx> {
    claims: Vec<ClaimPowRewardOp>,
    transactions: Vec<Tx>,
}

pub struct PoWService<Tx, TimeBackend, CryptarchiaService, RuntimeServiceId> {
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    state: PoWServiceState<Tx>,
    _phantom: PhantomData<(TimeBackend, CryptarchiaService)>,
}

impl<Tx, TimeBackend, CryptarchiaService, RuntimeServiceId> ServiceData
    for PoWService<Tx, TimeBackend, CryptarchiaService, RuntimeServiceId>
{
    type Settings = ();
    type State = PoWServiceState<Tx>;
    type StateOperator = ();
    type Message = PoWSerciveMessage;
}

#[async_trait::async_trait]
impl<Tx, TimeBackend, CryptarchiaService, RuntimeServiceId> ServiceCore<RuntimeServiceId>
    for PoWService<Tx, TimeBackend, CryptarchiaService, RuntimeServiceId>
where
    Tx: Send + Sync + 'static,
    TimeBackend: TimeBackendTrait + Send + Sync + 'static,
    TimeBackend::Settings: Clone + Send + Sync + 'static,
    CryptarchiaService: CryptarchiaServiceData<Tx = Tx> + 'static,
    RuntimeServiceId: Debug
        + Clone
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<Self>
        + AsServiceId<TimeService<TimeBackend, RuntimeServiceId>>
        + AsServiceId<CryptarchiaService>,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        initial_state: Self::State,
    ) -> Result<Self, DynError> {
        Ok(Self {
            service_resources_handle,
            state: initial_state,
            _phantom: PhantomData,
        })
    }

    async fn run(self) -> Result<(), DynError> {
        let Self {
            service_resources_handle,
            state: _state,
            _phantom,
        } = self;

        // Relay to the time service, used to subscribe to slot ticks / query the
        // current slot.
        let _time_relay: OutboundRelay<TimeServiceMessage> = service_resources_handle
            .overwatch_handle
            .relay::<TimeService<TimeBackend, _>>()
            .await
            .expect("Relay connection with TimeService should succeed");

        // API wrapper over the chain service relay, used to query chain state.
        let _cryptarchia_api = CryptarchiaServiceApi::<CryptarchiaService, RuntimeServiceId>::new(
            service_resources_handle
                .overwatch_handle
                .relay::<CryptarchiaService>()
                .await
                .expect("Relay connection with Cryptarchia chain service should succeed"),
        );

        service_resources_handle.status_updater.notify_ready();

        todo!()
    }
}
