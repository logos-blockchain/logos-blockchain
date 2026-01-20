use std::{
    fmt::{Debug, Display},
    hash::Hash,
    marker::PhantomData,
};

use chain_service::api::{CryptarchiaServiceApi, CryptarchiaServiceData};
use nomos_core::{
    da,
    header::HeaderId,
    mantle::{AuthenticatedMantleTx, TxHash},
};
use nomos_da_sampling::{
    DaSamplingService, backend::DaSamplingServiceBackend, mempool::DaMempoolAdapter,
};
use nomos_network::{NetworkService, message::BackendNetworkMsg};
use nomos_time::{TimeService, TimeServiceMessage, backends::TimeBackend as TimeBackendTrait};
use overwatch::{
    OpaqueServiceResourcesHandle,
    services::{AsServiceId, relay::OutboundRelay},
};
use serde::{Serialize, de::DeserializeOwned};
use tx_service::{
    MempoolMsg, TxMempoolService, backend::RecoverableMempool,
    network::NetworkAdapter as MempoolNetworkAdapter, storage::MempoolStorageAdapter,
};

use crate::{ChainNetwork, SamplingRelay, mempool::adapter::MempoolAdapter, network};

type NetworkRelay<NetworkBackend, RuntimeServiceId> =
    OutboundRelay<BackendNetworkMsg<NetworkBackend, RuntimeServiceId>>;
pub type TimeRelay = OutboundRelay<TimeServiceMessage>;

pub struct ChainNetworkRelays<
    Cryptarchia,
    Mempool,
    MempoolNetAdapter,
    MempoolDaAdapter,
    NetworkAdapter,
    SamplingBackend,
    RuntimeServiceId,
> where
    Cryptarchia: CryptarchiaServiceData<Tx: Send + Sync>,
    Mempool: RecoverableMempool<BlockId = HeaderId, Key = TxHash> + Send + Sync,
    MempoolNetAdapter: tx_service::network::NetworkAdapter<RuntimeServiceId>,
    MempoolDaAdapter: DaMempoolAdapter,
    NetworkAdapter: network::NetworkAdapter<RuntimeServiceId>,
    SamplingBackend: DaSamplingServiceBackend,
{
    cryptarchia: CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>,
    network_relay: NetworkRelay<NetworkAdapter::Backend, RuntimeServiceId>,
    mempool_adapter: MempoolAdapter<Mempool::Item>,
    sampling_relay: SamplingRelay<SamplingBackend::BlobId>,
    time_relay: TimeRelay,
    _mempool_adapter: PhantomData<MempoolNetAdapter>,
    _da_mempool_adapter: PhantomData<MempoolDaAdapter>,
}

impl<
    Cryptarchia,
    Mempool,
    MempoolNetAdapter,
    MempoolDaAdapter,
    NetworkAdapter,
    SamplingBackend,
    RuntimeServiceId,
>
    ChainNetworkRelays<
        Cryptarchia,
        Mempool,
        MempoolNetAdapter,
        MempoolDaAdapter,
        NetworkAdapter,
        SamplingBackend,
        RuntimeServiceId,
    >
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
    MempoolDaAdapter: DaMempoolAdapter + Send + Sync + 'static,
    NetworkAdapter: network::NetworkAdapter<RuntimeServiceId>,
    NetworkAdapter::Settings: Send,
    NetworkAdapter::PeerId: Clone + Eq + Hash + Send + Sync,
    SamplingBackend: DaSamplingServiceBackend<BlobId = da::BlobId> + Send,
    SamplingBackend::Settings: Clone,
    SamplingBackend::Share: Debug + 'static,
{
    pub const fn new(
        cryptarchia: CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>,
        network_relay: NetworkRelay<NetworkAdapter::Backend, RuntimeServiceId>,
        mempool_relay: OutboundRelay<MempoolMsg<HeaderId, Mempool::Item, Mempool::Item, TxHash>>,
        sampling_relay: SamplingRelay<SamplingBackend::BlobId>,
        time_relay: TimeRelay,
    ) -> Self {
        let mempool_adapter = MempoolAdapter::new(mempool_relay);
        Self {
            cryptarchia,
            network_relay,
            mempool_adapter,
            sampling_relay,
            time_relay,
            _mempool_adapter: PhantomData,
            _da_mempool_adapter: PhantomData,
        }
    }

    #[expect(clippy::allow_attributes_without_reason)]
    #[expect(clippy::type_complexity)]
    pub async fn from_service_resources_handle<
        SamplingNetworkAdapter,
        SamplingStorage,
        TimeBackend,
    >(
        service_resources_handle: &OpaqueServiceResourcesHandle<
            ChainNetwork<
                Cryptarchia,
                NetworkAdapter,
                Mempool,
                MempoolNetAdapter,
                MempoolDaAdapter,
                SamplingBackend,
                SamplingNetworkAdapter,
                SamplingStorage,
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
        SamplingNetworkAdapter:
            nomos_da_sampling::network::NetworkAdapter<RuntimeServiceId> + Send + Sync,
        SamplingStorage:
            nomos_da_sampling::storage::DaStorageAdapter<RuntimeServiceId> + Send + Sync,
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
            + AsServiceId<
                DaSamplingService<
                    SamplingBackend,
                    SamplingNetworkAdapter,
                    SamplingStorage,
                    MempoolDaAdapter,
                    RuntimeServiceId,
                >,
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

        let sampling_relay = service_resources_handle
            .overwatch_handle
            .relay::<DaSamplingService<_, _, _, _, _>>()
            .await
            .expect("Relay connection with SamplingService should succeed");

        let time_relay = service_resources_handle
            .overwatch_handle
            .relay::<TimeService<_, _>>()
            .await
            .expect("Relay connection with TimeService should succeed");

        Self::new(
            cryptarchia,
            network_relay,
            mempool_relay,
            sampling_relay,
            time_relay,
        )
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

    pub const fn sampling_relay(&self) -> &SamplingRelay<SamplingBackend::BlobId> {
        &self.sampling_relay
    }

    pub const fn time_relay(&self) -> &TimeRelay {
        &self.time_relay
    }
}
