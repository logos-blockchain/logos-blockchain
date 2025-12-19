use std::sync::Arc;

use nomos_core::codec::{DeserializeOp as _, SerializeOp as _};
use redb::{
    CommitError, Database, DatabaseError, ReadableDatabase as _, ReadableTable as _, StorageError,
    TableDefinition, TableError, TransactionError,
};
use thiserror::Error;
use tokio::sync::RwLock;

use crate::block::ValidatedL2Info;

const BLOCKS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("blocks");
const INVALID_BLOCKS_TABLE: TableDefinition<u64, ()> = TableDefinition::new("invalid_blocks");

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
        drop(write_txn.open_table(INVALID_BLOCKS_TABLE)?);
        write_txn.commit()?;

        Ok(Self {
            db: Arc::new(RwLock::new(db)),
        })
    }

    pub async fn add_block(&self, block: ValidatedL2Info) -> Result<(), DbError> {
        let serialized = block.to_bytes().unwrap();
        let write_txn = self.db.write().await.begin_write()?;
        write_txn
            .open_table(BLOCKS_TABLE)?
            .insert(block.as_ref().data.block_id, &*serialized)?;
        write_txn.commit()?;
        Ok(())
    }

    pub async fn mark_block_as_invalid(&self, block_id: u64) -> Result<(), DbError> {
        let write_txn = self.db.write().await.begin_write()?;
        write_txn
            .open_table(INVALID_BLOCKS_TABLE)?
            .insert(block_id, &())?;
        write_txn.commit()?;
        Ok(())
    }

    pub async fn unmark_block_as_invalid(&self, block_id: u64) -> Result<bool, DbError> {
        let write_txn = self.db.write().await.begin_write()?;
        let is_old_value_removed = write_txn
            .open_table(INVALID_BLOCKS_TABLE)?
            .remove(&block_id)?
            .is_some();
        write_txn.commit()?;
        Ok(is_old_value_removed)
    }

    pub async fn is_block_valid(&self, block_id: u64) -> Result<bool, DbError> {
        let read_txn = self.db.read().await.begin_read()?;
        let table = read_txn.open_table(INVALID_BLOCKS_TABLE)?;
        Ok(table.get(&block_id)?.is_none())
    }

    pub async fn get_all_blocks(&self) -> Result<Vec<ValidatedL2Info>, DbError> {
        let read_txn = self.db.read().await.begin_read()?;

        let deserialized_blocks: Vec<ValidatedL2Info> = read_txn
            .open_table(BLOCKS_TABLE)?
            .iter()?
            .filter_map(Result::ok)
            .map(|(_, value)| value)
            .map(|value| ValidatedL2Info::from_bytes(value.value()).unwrap())
            .collect();

        Ok(deserialized_blocks)
    }
}
