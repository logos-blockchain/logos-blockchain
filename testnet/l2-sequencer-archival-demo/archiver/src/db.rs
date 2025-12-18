use std::sync::Arc;

use nomos_core::codec::{DeserializeOp as _, SerializeOp as _};
use redb::{
    CommitError, Database, DatabaseError, ReadableDatabase as _, ReadableTable as _, StorageError,
    TableDefinition, TableError, TransactionError,
};
use thiserror::Error;
use tokio::sync::RwLock;

use crate::block::L2BlockInfo;

const BLOCKS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("blocks");

#[derive(Debug, Error)]
pub enum DbError {
    #[error("Database error: {0}")]
    Database(#[from] DatabaseError),
    #[error("Transaction error: {0}")]
    Transaction(#[from] Box<TransactionError>),
    #[error("Table error: {0}")]
    Table(#[from] TableError),
    #[error("Storage error: {0}")]
    Storage(#[from] StorageError),
    #[error("Commit error: {0}")]
    Commit(#[from] CommitError),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl From<TransactionError> for DbError {
    fn from(err: TransactionError) -> Self {
        Self::Transaction(Box::new(err))
    }
}

/// Persistent storage for blocks using redb
#[derive(Clone)]
pub struct BlockStore {
    db: Arc<RwLock<Database>>,
}

impl BlockStore {
    pub fn new(path: &str) -> Result<Self, DbError> {
        let db = Database::create(path)?;

        let write_txn = db.begin_write()?;
        drop(write_txn.open_table(BLOCKS_TABLE)?);
        write_txn.commit()?;

        Ok(Self {
            db: Arc::new(RwLock::new(db)),
        })
    }

    pub async fn add_block(&self, block: L2BlockInfo) -> Result<(), DbError> {
        let serialized = block.to_bytes().unwrap();
        let write_txn = self.db.write().await.begin_write()?;
        write_txn
            .open_table(BLOCKS_TABLE)?
            .insert(block.data.block_id, &*serialized)?;
        write_txn.commit()?;
        Ok(())
    }

    pub async fn get_all_blocks(&self) -> Result<Vec<L2BlockInfo>, DbError> {
        let read_txn = self.db.read().await.begin_read()?;

        let deserialized_blocks: Vec<L2BlockInfo> = read_txn
            .open_table(BLOCKS_TABLE)?
            .iter()?
            .filter_map(Result::ok)
            .map(|(_, value)| value)
            .map(|value| L2BlockInfo::from_bytes(value.value()).unwrap())
            .collect();

        Ok(deserialized_blocks)
    }
}
