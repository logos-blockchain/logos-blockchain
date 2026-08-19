use core::fmt::{Debug, Display};
use std::marker::PhantomData;

use lb_blend_service::api::{BlendServiceApi, BlendServiceData};
use lb_chain_service::api::{CryptarchiaServiceApi, CryptarchiaServiceData};
use lb_core::mantle::ops::pow::ClaimPowRewardOp;
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{AsServiceId, ServiceCore, ServiceData},
};

pub enum PoWServiceMessage {}

pub struct PoWServiceState<Tx> {
    claims: Vec<ClaimPowRewardOp>,
    transactions: Vec<Tx>,
}

pub struct PoWService<Tx, CryptarchiaService, BlendService, RuntimeServiceId> {
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    state: PoWServiceState<Tx>,
    _phantom: PhantomData<(CryptarchiaService, BlendService)>,
}

impl<Tx, CryptarchiaService, BlendService, RuntimeServiceId> ServiceData
    for PoWService<Tx, CryptarchiaService, BlendService, RuntimeServiceId>
{
    type Settings = ();
    type State = PoWServiceState<Tx>;
    type StateOperator = ();
    type Message = PoWServiceMessage;
}

#[async_trait::async_trait]
impl<Tx, CryptarchiaService, BlendService, RuntimeServiceId> ServiceCore<RuntimeServiceId>
    for PoWService<Tx, CryptarchiaService, BlendService, RuntimeServiceId>
where
    Tx: Send + Sync + 'static,
    CryptarchiaService: CryptarchiaServiceData<Tx = Tx> + 'static,
    BlendService: BlendServiceData,
    BlendService::NodeId: Send,
    <BlendService as ServiceData>::Message: Send + 'static,
    RuntimeServiceId: Debug
        + Clone
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<Self>
        + AsServiceId<CryptarchiaService>
        + AsServiceId<BlendService>,
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

        // API wrapper over the blend service relay, used to publish PoW payloads
        // to the blend network and query blend state.
        let _blend_api = BlendServiceApi::<BlendService, RuntimeServiceId>::new(
            service_resources_handle
                .overwatch_handle
                .relay::<BlendService>()
                .await
                .expect("Relay connection with BlendService should succeed"),
        );

        service_resources_handle.status_updater.notify_ready();

        Ok(())
    }
}
