use std::{collections::BTreeSet, pin::Pin};

use async_trait::async_trait;
use futures::Stream;
use lb_storage_service::backends::StorageBackend;

pub mod adapters;

#[async_trait]
pub trait MempoolStorageAdapter<RuntimeServiceId>: Send + Sync {
    type Backend: StorageBackend + Send + Sync + 'static;

    type Item: Send;

    type Key: Send + Sync;

    type Error: Send;

    fn new(
        storage_relay: overwatch::services::relay::OutboundRelay<
            <lb_storage_service::StorageService<Self::Backend, RuntimeServiceId> as overwatch::services::ServiceData>::Message,
        >,
    ) -> Self;

    async fn store_item(&mut self, key: Self::Key, item: Self::Item) -> Result<(), Self::Error>;

    async fn get_items(
        &self,
        keys: &BTreeSet<Self::Key>,
    ) -> Result<Pin<Box<dyn Stream<Item = Self::Item> + Send>>, Self::Error>;

    async fn remove_items(&mut self, keys: &[Self::Key]) -> Result<(), Self::Error>;
}
