use lb_core::{block::Block, header::HeaderId, mantle::TxHash};
use lb_storage_service::{StorageService, backends::rocksdb::RocksBackend};
use overwatch::services::{ServiceData, relay::OutboundRelay};
use serde::{Serialize, de::DeserializeOwned};

pub mod adapters;

#[async_trait::async_trait]
pub trait StorageAdapter<RuntimeServiceId> {
    async fn get_block<Tx>(
        storage_relay: OutboundRelay<
            <StorageService<RocksBackend, RuntimeServiceId> as ServiceData>::Message,
        >,
        id: HeaderId,
    ) -> Result<Option<Block<Tx>>, crate::http::DynError>
    where
        Tx: Serialize + DeserializeOwned + Clone + Eq + 'static;

    async fn get_transactions<Tx>(
        storage_relay: OutboundRelay<
            <StorageService<RocksBackend, RuntimeServiceId> as ServiceData>::Message,
        >,
        id: TxHash,
    ) -> Result<Vec<Tx>, crate::http::DynError>
    where
        Tx: DeserializeOwned + Send;
}
