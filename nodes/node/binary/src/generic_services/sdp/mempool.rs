use std::{
    fmt::{Debug, Display},
    marker::PhantomData,
};

use lb_core::{
    header::HeaderId,
    mantle::{
        MantleTransaction,
        traits::Hashable as _,
        transactions::{hash::TxHash, states::Preverified},
    },
};
use lb_sdp_service::mempool::{MempoolAdapterError, SdpMempoolAdapter as SdpMempoolAdapterTrait};
use lb_storage_service::StorageService;
use lb_tx_service::{
    MempoolMsg, TxMempoolService,
    backend::{MemPool, RecoverableMempool},
    network::NetworkAdapter as MempoolNetworkAdapter,
    storage::MempoolStorageAdapter,
};
use overwatch::services::{AsServiceId, ServiceData, relay::OutboundRelay};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

type MempoolRelay<Item, Key> = OutboundRelay<MempoolMsg<HeaderId, Item, Item, Key>>;

pub struct SdpMempoolAdapter<MempoolNetAdapter, Mempool, RuntimeServiceId>
where
    Mempool: MemPool<BlockId = HeaderId, Key = TxHash>,
    MempoolNetAdapter: MempoolNetworkAdapter<RuntimeServiceId, Key = Mempool::Key>,
    Mempool::Item: Clone + Eq + Debug + 'static,
    Mempool::Key: Debug + 'static,
{
    pub mempool_relay: MempoolRelay<Mempool::Item, Mempool::Key>,
    _phantom: PhantomData<(MempoolNetAdapter, RuntimeServiceId)>,
}

#[async_trait::async_trait]
impl<MempoolNetAdapter, Mempool, RuntimeServiceId> SdpMempoolAdapterTrait
    for SdpMempoolAdapter<MempoolNetAdapter, Mempool, RuntimeServiceId>
where
    Mempool: RecoverableMempool<BlockId = HeaderId, Key = TxHash, Item = MantleTransaction<Preverified>>
        + Send
        + Sync,
    Mempool::RecoveryState: Serialize + for<'de> Deserialize<'de>,
    Mempool::Settings: Clone + Send + Sync,
    Mempool::Storage: MempoolStorageAdapter<RuntimeServiceId> + Send + Sync + Clone,
    MempoolNetAdapter: MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>
        + Send
        + Sync,
    MempoolNetAdapter::Settings: Send + Sync,
    RuntimeServiceId: Clone
        + Debug
        + Display
        + Send
        + Sync
        + 'static
        + AsServiceId<
            StorageService<
                <Mempool::Storage as MempoolStorageAdapter<RuntimeServiceId>>::Backend,
                RuntimeServiceId,
            >,
        >,
{
    type MempoolService =
        TxMempoolService<MempoolNetAdapter, Mempool, Mempool::Storage, RuntimeServiceId>;
    type Tx = MantleTransaction<Preverified>;

    fn new(mempool_relay: OutboundRelay<<Self::MempoolService as ServiceData>::Message>) -> Self {
        Self {
            mempool_relay,
            _phantom: PhantomData,
        }
    }

    async fn post_tx(&self, tx: Self::Tx) -> Result<(), MempoolAdapterError> {
        let (reply_channel, receiver) = oneshot::channel();
        self.mempool_relay
            .send(MempoolMsg::Add {
                key: tx.hash(),
                payload: tx,
                reply_channel,
            })
            .await
            .map_err(|(e, _)| MempoolAdapterError::Other(Box::new(e)))?;

        receiver
            .await?
            .map_err(|e| MempoolAdapterError::Mempool(Box::new(e)))
    }
}
