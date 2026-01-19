use lb_core::header;

use crate::{api::StorageBackendApi, backends::rocksdb::RocksBackend};

pub mod chain;
pub mod da;
pub mod membership;
pub mod utils;

impl StorageBackendApi for RocksBackend {}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("RocksDB error: {0}")]
    RocksDbError(#[from] rocksdb::Error),
    #[error("Block header error: {0}")]
    BlockHeaderError(#[from] header::Error),
}
