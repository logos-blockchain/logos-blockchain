use std::{fmt::Debug, marker::PhantomData};

use lb_core::{
    header::HeaderId,
    mantle::{SignedMantleTx, Transaction as _, TxHash},
};
use lb_tx_service::{
    MempoolMsg, TxMempoolService,
    backend::{MemPool, RecoverableMempool},
    network::NetworkAdapter as MempoolNetworkAdapter,
    storage::MempoolStorageAdapter,
};
use overwatch::services::{ServiceData, relay::OutboundRelay};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use super::{DaMempoolAdapter, MempoolAdapterError};

type MempoolRelay<Item, Key> = OutboundRelay<MempoolMsg<HeaderId, Item, Item, Key>>;

pub struct KzgrsMempoolNetworkAdapter<MempoolNetAdapter, Mempool, RuntimeServiceId>
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
impl<MempoolNetAdapter, Mempool, RuntimeServiceId> DaMempoolAdapter
    for KzgrsMempoolNetworkAdapter<MempoolNetAdapter, Mempool, RuntimeServiceId>
where
    Mempool:
        RecoverableMempool<BlockId = HeaderId, Key = TxHash, Item = SignedMantleTx> + Send + Sync,
    Mempool::RecoveryState: Serialize + for<'de> Deserialize<'de>,
    Mempool::Settings: Clone + Send + Sync,
    Mempool::Storage: MempoolStorageAdapter<RuntimeServiceId> + Send + Sync + Clone,
    MempoolNetAdapter: MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>
        + Send
        + Sync,
    MempoolNetAdapter::Settings: Send + Sync,
    RuntimeServiceId: Send + Sync,
{
    type MempoolService =
        TxMempoolService<MempoolNetAdapter, Mempool, Mempool::Storage, RuntimeServiceId>;
    type Tx = SignedMantleTx;

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

        receiver.await?.map_err(MempoolAdapterError::Mempool)
    }
}
