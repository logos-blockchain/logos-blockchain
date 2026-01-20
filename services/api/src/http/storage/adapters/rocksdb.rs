use std::{
    fmt::{Debug, Display},
    marker::PhantomData,
};

use bytes::Bytes;
use lb_core::{block::Block, header::HeaderId};
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
}
