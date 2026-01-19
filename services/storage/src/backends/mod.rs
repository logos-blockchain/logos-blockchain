#[cfg(feature = "rocksdb-backend")]
pub mod rocksdb;

use std::{error::Error, num::NonZeroUsize};

use async_trait::async_trait;
use bytes::Bytes;
pub use lb_core::codec::SerdeOp;

use crate::api::StorageBackendApi;

/// Trait to abstract storage transactions return and operation types
pub trait StorageTransaction: Send + Sync {
    type Result: Send + Sync;
    type Transaction: Send + Sync;
}

/// Main storage functionality trait
#[async_trait]
pub trait StorageBackend: StorageBackendApi + Sized {
    /// Backend settings
    type Settings: Clone + Send + Sync + 'static;
    /// Backend operations error type
    type Error: Error + 'static + Send + Sync;
    /// Backend transaction type
    /// Usually it will be some function that modifies the storage directly or
    /// operates over the backend as per the backend specification.
    type Transaction: StorageTransaction;
    fn new(config: Self::Settings) -> Result<Self, <Self as StorageBackend>::Error>;
    async fn store(
        &mut self,
        key: Bytes,
        value: Bytes,
    ) -> Result<(), <Self as StorageBackend>::Error>;
    async fn bulk_store<I>(&mut self, items: I) -> Result<(), <Self as StorageBackend>::Error>
    where
        I: IntoIterator<Item = (Bytes, Bytes)> + Send + 'static;
    async fn load(&mut self, key: &[u8]) -> Result<Option<Bytes>, <Self as StorageBackend>::Error>;
    /// Loads all values whose keys start with the given prefix.
    async fn load_prefix(
        &mut self,
        prefix: &[u8],
        start_key: Option<&[u8]>,
        end_key: Option<&[u8]>,
        limit: Option<NonZeroUsize>,
    ) -> Result<Vec<Bytes>, <Self as StorageBackend>::Error>;
    async fn remove(
        &mut self,
        key: &[u8],
    ) -> Result<Option<Bytes>, <Self as StorageBackend>::Error>;
    /// Execute a transaction in the current backend
    async fn execute(
        &mut self,
        transaction: Self::Transaction,
    ) -> Result<<Self::Transaction as StorageTransaction>::Result, <Self as StorageBackend>::Error>;
}
