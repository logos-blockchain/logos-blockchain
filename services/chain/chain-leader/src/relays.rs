use core::fmt::Display;
use std::fmt::Debug;

use lb_chain_service::api::CryptarchiaServiceData;
use lb_core::{
    header::HeaderId,
    mantle::{AuthenticatedMantleTx, TxHash},
};
use lb_time_service::{TimeService, TimeServiceMessage, backends::TimeBackend as TimeBackendTrait};
use lb_tx_service::{
    MempoolMsg, TxMempoolService, backend::RecoverableMempool,
    network::NetworkAdapter as MempoolNetworkAdapter, storage::MempoolStorageAdapter,
};
use overwatch::{
    OpaqueServiceResourcesHandle,
    services::{AsServiceId, ServiceData, relay::OutboundRelay},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::mempool::adapter;

type BlendRelay<BlendService> = OutboundRelay<<BlendService as ServiceData>::Message>;
type TimeRelay = OutboundRelay<TimeServiceMessage>;

pub struct CryptarchiaConsensusRelays<BlendService, Mempool, MempoolNetAdapter, RuntimeServiceId>
where
    BlendService: ServiceData,
    Mempool: RecoverableMempool<BlockId = HeaderId, Key = TxHash>,
    MempoolNetAdapter: MempoolNetworkAdapter<RuntimeServiceId>,
{
    blend_relay: BlendRelay<BlendService>,
    mempool_adapter: adapter::MempoolAdapter<Mempool::Item>,
    time_relay: TimeRelay,
    _mempool_adapter: std::marker::PhantomData<(MempoolNetAdapter, RuntimeServiceId)>,
}

impl<BlendService, Mempool, MempoolNetAdapter, RuntimeServiceId> Clone
    for CryptarchiaConsensusRelays<BlendService, Mempool, MempoolNetAdapter, RuntimeServiceId>
where
    BlendService: ServiceData,
    Mempool: RecoverableMempool<BlockId = HeaderId, Key = TxHash>,
    MempoolNetAdapter: MempoolNetworkAdapter<RuntimeServiceId>,
{
    fn clone(&self) -> Self {
        Self {
            blend_relay: self.blend_relay.clone(),
            mempool_adapter: self.mempool_adapter.clone(),
            time_relay: self.time_relay.clone(),
            _mempool_adapter: std::marker::PhantomData,
        }
    }
}

impl<BlendService, Mempool, MempoolNetAdapter, RuntimeServiceId>
    CryptarchiaConsensusRelays<BlendService, Mempool, MempoolNetAdapter, RuntimeServiceId>
where
    BlendService: ServiceData,
    Mempool: Send + Sync + RecoverableMempool<BlockId = HeaderId, Key = TxHash>,
    Mempool::Storage: MempoolStorageAdapter<RuntimeServiceId> + Clone + Send + Sync,
    Mempool::RecoveryState: Serialize + for<'de> Deserialize<'de>,
    Mempool::Item: Debug
        + Serialize
        + DeserializeOwned
        + Eq
        + Clone
        + Send
        + Sync
        + 'static
        + AuthenticatedMantleTx,
    Mempool::Settings: Clone,
    MempoolNetAdapter: MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>
        + Send
        + Sync,
    MempoolNetAdapter::Settings: Send + Sync,
{
    pub const fn new(
        blend_relay: BlendRelay<BlendService>,
        mempool_relay: OutboundRelay<MempoolMsg<HeaderId, Mempool::Item, Mempool::Item, TxHash>>,
        time_relay: TimeRelay,
    ) -> Self {
        let mempool_adapter = adapter::MempoolAdapter::new(mempool_relay);
        Self {
            blend_relay,
            mempool_adapter,
            time_relay,
            _mempool_adapter: std::marker::PhantomData,
        }
    }

    #[expect(clippy::allow_attributes_without_reason)]
    pub async fn from_service_resources_handle<S, TimeBackend, CryptarchiaService>(
        service_resources_handle: &OpaqueServiceResourcesHandle<S, RuntimeServiceId>,
    ) -> Self
    where
        S: ServiceData,
        <S as ServiceData>::Message: Send + Sync + 'static,
        <S as ServiceData>::Settings: Send + Sync + 'static,
        <S as ServiceData>::State: Send + Sync + 'static,
        Mempool::Key: Send,
        Mempool::Storage: MempoolStorageAdapter<RuntimeServiceId> + Clone + Send + Sync,
        Mempool::Settings: Sync,
        BlendService: lb_blend_service::ServiceComponents,
        BlendService::BroadcastSettings: Send + Sync,
        <BlendService as ServiceData>::Message: Send + 'static,
        TimeBackend: TimeBackendTrait,
        TimeBackend::Settings: Clone + Send + Sync + 'static,
        RuntimeServiceId: Debug
            + Sync
            + Send
            + Display
            + 'static
            + AsServiceId<BlendService>
            + AsServiceId<
                TxMempoolService<MempoolNetAdapter, Mempool, Mempool::Storage, RuntimeServiceId>,
            >
            + AsServiceId<TimeService<TimeBackend, RuntimeServiceId>>
            + AsServiceId<CryptarchiaService>,
        CryptarchiaService: CryptarchiaServiceData<Tx = Mempool::Item>,
    {
        let blend_relay = service_resources_handle
            .overwatch_handle
            .relay::<BlendService>()
            .await
            .expect(
                "Relay connection with lb_blend_service::BlendService should
        succeed",
            );

        let mempool_relay = service_resources_handle
            .overwatch_handle
            .relay::<TxMempoolService<_, _, _, _>>()
            .await
            .expect("Relay connection with MempoolService should succeed");

        let time_relay = service_resources_handle
            .overwatch_handle
            .relay::<TimeService<_, _>>()
            .await
            .expect("Relay connection with TimeService should succeed");

        Self::new(blend_relay, mempool_relay, time_relay)
    }

    pub const fn blend_relay(&self) -> &BlendRelay<BlendService> {
        &self.blend_relay
    }

    pub const fn mempool_adapter(&self) -> &adapter::MempoolAdapter<Mempool::Item> {
        &self.mempool_adapter
    }

    pub const fn time_relay(&self) -> &TimeRelay {
        &self.time_relay
    }
}
