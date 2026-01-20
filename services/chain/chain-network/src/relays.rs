use std::{
    fmt::{Debug, Display},
    hash::Hash,
    marker::PhantomData,
};

use lb_chain_service::api::{CryptarchiaServiceApi, CryptarchiaServiceData};
use lb_core::{
    header::HeaderId,
    mantle::{AuthenticatedMantleTx, TxHash},
};
use lb_network_service::{NetworkService, message::BackendNetworkMsg};
use lb_time_service::{TimeService, backends::TimeBackend as TimeBackendTrait};
use lb_tx_service::{
    MempoolMsg, TxMempoolService, backend::RecoverableMempool,
    network::NetworkAdapter as MempoolNetworkAdapter, storage::MempoolStorageAdapter,
};
use overwatch::{
    OpaqueServiceResourcesHandle,
    services::{AsServiceId, relay::OutboundRelay},
};
use serde::{Serialize, de::DeserializeOwned};

use crate::{ChainNetwork, mempool::adapter::MempoolAdapter, network};

type NetworkRelay<NetworkBackend, RuntimeServiceId> =
    OutboundRelay<BackendNetworkMsg<NetworkBackend, RuntimeServiceId>>;

pub struct ChainNetworkRelays<
    Cryptarchia,
    Mempool,
    MempoolNetAdapter,
    NetworkAdapter,
    RuntimeServiceId,
> where
    Cryptarchia: CryptarchiaServiceData<Tx: Send + Sync>,
    Mempool: RecoverableMempool<BlockId = HeaderId, Key = TxHash> + Send + Sync,
    MempoolNetAdapter: lb_tx_service::network::NetworkAdapter<RuntimeServiceId>,
    NetworkAdapter: network::NetworkAdapter<RuntimeServiceId>,
{
    cryptarchia: CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>,
    network_relay: NetworkRelay<NetworkAdapter::Backend, RuntimeServiceId>,
    mempool_adapter: MempoolAdapter<Mempool::Item>,
    _mempool_adapter: PhantomData<MempoolNetAdapter>,
}

impl<Cryptarchia, Mempool, MempoolNetAdapter, NetworkAdapter, RuntimeServiceId>
    ChainNetworkRelays<Cryptarchia, Mempool, MempoolNetAdapter, NetworkAdapter, RuntimeServiceId>
where
    Cryptarchia: CryptarchiaServiceData<Tx: Send + Sync>,
    Mempool: RecoverableMempool<BlockId = HeaderId, Key = TxHash> + Send + Sync,
    Mempool::RecoveryState: Serialize + DeserializeOwned,
    Mempool::Item: Debug
        + Serialize
        + DeserializeOwned
        + Eq
        + Clone
        + Send
        + Sync
        + 'static
        + AuthenticatedMantleTx,
    Mempool::Settings: Clone + Send + Sync,
    Mempool::Storage: MempoolStorageAdapter<RuntimeServiceId> + Clone + Send + Sync,
    MempoolNetAdapter: MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>
        + Send
        + Sync,
    MempoolNetAdapter::Settings: Send + Sync,
    NetworkAdapter: network::NetworkAdapter<RuntimeServiceId>,
    NetworkAdapter::Settings: Send,
    NetworkAdapter::PeerId: Clone + Eq + Hash + Send + Sync,
{
    pub const fn new(
        cryptarchia: CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>,
        network_relay: NetworkRelay<NetworkAdapter::Backend, RuntimeServiceId>,
        mempool_relay: OutboundRelay<MempoolMsg<HeaderId, Mempool::Item, Mempool::Item, TxHash>>,
    ) -> Self {
        let mempool_adapter = MempoolAdapter::new(mempool_relay);
        Self {
            cryptarchia,
            network_relay,
            mempool_adapter,
            _mempool_adapter: PhantomData,
        }
    }

    #[expect(clippy::allow_attributes_without_reason)]
    pub async fn from_service_resources_handle<TimeBackend>(
        service_resources_handle: &OpaqueServiceResourcesHandle<
            ChainNetwork<
                Cryptarchia,
                NetworkAdapter,
                Mempool,
                MempoolNetAdapter,
                TimeBackend,
                RuntimeServiceId,
            >,
            RuntimeServiceId,
        >,
    ) -> Self
    where
        Cryptarchia: CryptarchiaServiceData<Tx = Mempool::Item>,
        Mempool::Key: Send,
        NetworkAdapter::Settings: Sync + Send,
        TimeBackend: TimeBackendTrait,
        TimeBackend::Settings: Clone + Send + Sync,
        RuntimeServiceId: Debug
            + Sync
            + Send
            + Display
            + 'static
            + AsServiceId<Cryptarchia>
            + AsServiceId<NetworkService<NetworkAdapter::Backend, RuntimeServiceId>>
            + AsServiceId<
                TxMempoolService<MempoolNetAdapter, Mempool, Mempool::Storage, RuntimeServiceId>,
            >
            + AsServiceId<TimeService<TimeBackend, RuntimeServiceId>>,
    {
        let cryptarchia = CryptarchiaServiceApi::<Cryptarchia, _>::new(
            service_resources_handle
                .overwatch_handle
                .relay::<Cryptarchia>()
                .await
                .expect("Relay connection with Cryptarchia should succeed"),
        );
        let network_relay = service_resources_handle
            .overwatch_handle
            .relay::<NetworkService<_, _>>()
            .await
            .expect("Relay connection with NetworkService should succeed");

        let mempool_relay = service_resources_handle
            .overwatch_handle
            .relay::<TxMempoolService<_, _, _, _>>()
            .await
            .expect("Relay connection with MempoolService should succeed");

        Self::new(cryptarchia, network_relay, mempool_relay)
    }

    pub const fn cryptarchia(&self) -> &CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId> {
        &self.cryptarchia
    }

    pub const fn network_relay(&self) -> &NetworkRelay<NetworkAdapter::Backend, RuntimeServiceId> {
        &self.network_relay
    }

    pub const fn mempool_adapter(&self) -> &MempoolAdapter<Mempool::Item> {
        &self.mempool_adapter
    }
}
