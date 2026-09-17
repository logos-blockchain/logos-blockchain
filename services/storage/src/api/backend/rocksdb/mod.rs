use lb_core::header;

pub mod chain;
pub mod utils;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("RocksDB error: {0}")]
    RocksDbError(#[from] rocksdb::Error),
    #[error("Block header error: {0}")]
    BlockHeaderError(#[from] header::Error),
}
