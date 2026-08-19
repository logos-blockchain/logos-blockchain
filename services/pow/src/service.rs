use core::fmt::{Debug, Display};
use std::marker::PhantomData;

use lb_chain_service::api::{CryptarchiaServiceApi, CryptarchiaServiceData};
use lb_core::mantle::ops::pow::ClaimPowRewardOp;
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{AsServiceId, ServiceCore, ServiceData},
};

#[derive(thiserror::Error, Debug)]
pub enum PoWServiceError {}

pub enum PoWSerciveMessage {}

pub struct PoWServiceState<Tx> {
    claims: Vec<ClaimPowRewardOp>,
    transactions: Vec<Tx>,
}

pub struct PoWService<Tx, CryptarchiaService, RuntimeServiceId> {
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    state: PoWServiceState<Tx>,
    _phantom: PhantomData<CryptarchiaService>,
}

impl<Tx, CryptarchiaService, RuntimeServiceId> ServiceData
    for PoWService<Tx, CryptarchiaService, RuntimeServiceId>
{
    type Settings = ();
    type State = PoWServiceState<Tx>;
    type StateOperator = ();
    type Message = PoWSerciveMessage;
}

#[async_trait::async_trait]
impl<Tx, CryptarchiaService, RuntimeServiceId> ServiceCore<RuntimeServiceId>
    for PoWService<Tx, CryptarchiaService, RuntimeServiceId>
where
    Tx: Send + Sync + 'static,
    CryptarchiaService: CryptarchiaServiceData<Tx = Tx> + 'static,
    RuntimeServiceId: Debug
        + Clone
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<Self>
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

        // API wrapper over the chain service relay, used to query chain state.
        let cryptarchia_api = CryptarchiaServiceApi::<CryptarchiaService, RuntimeServiceId>::new(
            service_resources_handle
                .overwatch_handle
                .relay::<CryptarchiaService>()
                .await
                .expect("Relay connection with Cryptarchia chain service should succeed"),
        );
        cryptarchia_api.subscribe_new_blocks().await?;

        service_resources_handle.status_updater.notify_ready();

        Ok(())
    }
}
