use std::{
    collections::BTreeSet,
    fmt::{Debug, Display},
    marker::PhantomData,
};

use bytes::Bytes;
use futures::{StreamExt as _, TryStreamExt as _};
use lb_core::{block::Block, header::HeaderId, mantle::TxHash};
use lb_storage_service::{StorageMsg, StorageService, backends::rocksdb::RocksBackend};
use overwatch::services::{ServiceData, relay::OutboundRelay};
use serde::{Serialize, de::DeserializeOwned};

use crate::http::storage::StorageAdapter;

pub struct RocksAdapter<RuntimeServiceId> {
    _runtime_service_id: PhantomData<RuntimeServiceId>,
}

#[async_trait::async_trait]
impl<RuntimeServiceId> StorageAdapter<RuntimeServiceId> for RocksAdapter<RuntimeServiceId>
where
    RuntimeServiceId: Debug + Sync + Display + 'static,
{
    async fn get_block<Tx>(
        storage_relay: OutboundRelay<
            <StorageService<RocksBackend, RuntimeServiceId> as ServiceData>::Message,
        >,
        id: HeaderId,
    ) -> Result<Option<Block<Tx>>, crate::http::DynError>
    where
        Tx: Serialize + DeserializeOwned + Clone + Eq + 'static,
    {
        let key: [u8; 32] = id.into();
        let (msg, receiver) = StorageMsg::new_load_message(Bytes::copy_from_slice(&key));
        storage_relay.send(msg).await.map_err(|(e, _)| e)?;

        receiver
            .recv()
            .await
            .map_err(|e| Box::new(e) as crate::http::DynError)
    }

    async fn get_transactions<Tx>(
        storage_relay: OutboundRelay<
            <StorageService<RocksBackend, RuntimeServiceId> as ServiceData>::Message,
        >,
        id: TxHash,
    ) -> Result<Vec<Tx>, crate::http::DynError>
    where
        Tx: DeserializeOwned + Send,
    {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let message = StorageMsg::get_transactions_request(BTreeSet::from([id]), sender);
        storage_relay
            .send(message)
            .await
            .map_err(|(error, _)| error)?;

        let bytes_stream = receiver
            .await
            .map_err(|error| Box::new(error) as crate::http::DynError)?;

        bytes_stream
            .map(|bytes| {
                serde_json::from_slice::<Tx>(bytes.as_ref())
                    .map_err(|error| Box::new(error) as crate::http::DynError)
            })
            .try_collect::<Vec<_>>()
            .await
    }
}
