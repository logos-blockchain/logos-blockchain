use std::{fmt::Debug, marker::PhantomData, pin::Pin};

use futures::{Stream, StreamExt as _};
use nomos_core::{
    header::HeaderId,
    mantle::{Op, SignedMantleTx, TxHash},
};
use overwatch::services::{ServiceData, relay::OutboundRelay};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tx_service::{
    MempoolMsg, TxMempoolService,
    backend::{MemPool, RecoverableMempool},
    network::NetworkAdapter as MempoolNetworkAdapter,
    storage::MempoolStorageAdapter,
};

use super::{DaMempoolAdapter, MempoolAdapterError};
use crate::mempool::Blob;

type MempoolRelay<Item, Key> = OutboundRelay<MempoolMsg<HeaderId, Item, Item, Key>>;

pub struct SamplingMempoolNetworkAdapter<MempoolNetAdapter, Mempool, RuntimeServiceId>
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
    for SamplingMempoolNetworkAdapter<MempoolNetAdapter, Mempool, RuntimeServiceId>
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

    async fn subscribe(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = Blob> + Send>>, MempoolAdapterError> {
        let (reply_channel, receiver) = oneshot::channel();
        self.mempool_relay
            .send(MempoolMsg::Subscribe { reply_channel })
            .await
            .map_err(|(e, _)| MempoolAdapterError::Other(Box::new(e)))?;

        let rx = receiver
            .await
            .map_err(|e| MempoolAdapterError::Other(Box::new(e)))?;

        // Filter and map to extract blob IDs from DA blob operations
        let blob_stream = tokio_stream::wrappers::BroadcastStream::new(rx)
            .filter_map(async |result| result.ok())
            .flat_map(|tx: Self::Tx| {
                let blob_ids_iter = tx.mantle_tx.ops.into_iter().filter_map(|op| {
                    if let Op::ChannelBlob(blob_op) = op {
                        Some(Blob {
                            blob_id: blob_op.blob,
                            session: blob_op.session,
                        })
                    } else {
                        None
                    }
                });

                tokio_stream::iter(blob_ids_iter)
            });

        Ok(Box::pin(blob_stream))
    }
}
