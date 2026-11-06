use std::{error::Error, num::NonZeroUsize};

use bytes::Bytes;
pub use lb_binary_codec::bincode::SerdeOp;

/// Trait to abstract storage transactions return and operation types
pub trait StorageTransaction: Send + Sync {
    type Result: Send + Sync;
    type Transaction: Send + Sync;
}

/// Main storage functionality trait
///
/// Backends do their I/O in place, so an async caller has to run them on the
/// blocking pool.
pub trait StorageBackend: Sized {
    /// Backend settings
    type Settings: Clone + Send + Sync + 'static;
    /// Backend operations error type
    type Error: Error + 'static + Send + Sync;
    /// Backend transaction type
    /// Usually it will be some function that modifies the storage directly or
    /// operates over the backend as per the backend specification.
    type Transaction: StorageTransaction;
    fn new(config: Self::Settings) -> Result<Self, <Self as StorageBackend>::Error>;
    fn store(&self, key: Bytes, value: Bytes) -> Result<(), <Self as StorageBackend>::Error>;
    fn bulk_store<I>(&self, items: I) -> Result<(), <Self as StorageBackend>::Error>
    where
        I: IntoIterator<Item = (Bytes, Bytes)>;
    fn load(&self, key: &[u8]) -> Result<Option<Bytes>, <Self as StorageBackend>::Error>;
    /// Loads all values whose keys start with the given prefix.
    fn load_prefix(
        &self,
        prefix: &[u8],
        start_key: Option<&[u8]>,
        end_key: Option<&[u8]>,
        limit: Option<NonZeroUsize>,
    ) -> Result<Vec<Bytes>, <Self as StorageBackend>::Error>;
    /// Loads all values whose keys start with the given prefix in reverse
    /// order.
    fn load_prefix_reverse(
        &self,
        prefix: &[u8],
        start_key: Option<&[u8]>,
        end_key: Option<&[u8]>,
        limit: Option<NonZeroUsize>,
    ) -> Result<Vec<Bytes>, <Self as StorageBackend>::Error>;
    fn remove(&self, key: &[u8]) -> Result<Option<Bytes>, <Self as StorageBackend>::Error>;
    /// Execute a transaction in the current backend
    fn execute(
        &self,
        transaction: Self::Transaction,
    ) -> Result<<Self::Transaction as StorageTransaction>::Result, <Self as StorageBackend>::Error>;
}
