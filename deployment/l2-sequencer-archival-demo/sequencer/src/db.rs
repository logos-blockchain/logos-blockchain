use core::iter::once;
use std::sync::Arc;

use redb::{Database, ReadableTable as _, ReadableTableMetadata as _, TableDefinition};
use thiserror::Error;
use tokio::sync::RwLock;

const ACCOUNTS_TABLE: TableDefinition<&str, u64> = TableDefinition::new("accounts");
const STATE_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("state");
const COUNTER_TABLE: TableDefinition<&str, u64> = TableDefinition::new("counters");
// Queue table: key is tx_id, value is JSON-serialized PendingTransfer
const QUEUE_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("queue");
// Transactions table: key is tx_id, value is JSON-serialized Transaction
const TRANSACTIONS_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("transactions");
const LAST_MSG_ID_KEY: &str = "last_msg_id";
const BLOCK_ID_KEY: &str = "block_id";
const TX_INDEX_KEY: &str = "tx_index";

#[derive(Debug, Error)]
pub enum DbError {
    #[error("Database error: {0}")]
    Database(#[from] redb::DatabaseError),
    #[error("Transaction error: {0}")]
    Transaction(#[from] Box<redb::TransactionError>),
    #[error("Table error: {0}")]
    Table(#[from] redb::TableError),
    #[error("Storage error: {0}")]
    Storage(#[from] redb::StorageError),
    #[error("Commit error: {0}")]
    Commit(#[from] redb::CommitError),
    #[error("Insufficient balance: account {account} has {balance}, needs {required}")]
    InsufficientBalance {
        account: String,
        balance: u64,
        required: u64,
    },
    #[error("Cannot transfer to self: {account}")]
    SelfTransfer { account: String },
}

impl From<redb::TransactionError> for DbError {
    fn from(err: redb::TransactionError) -> Self {
        Self::Transaction(Box::new(err))
    }
}

pub type Result<T> = std::result::Result<T, DbError>;

#[derive(Clone)]
pub struct AccountDb {
    db: Arc<RwLock<Database>>,
    initial_balance: u64,
}

impl AccountDb {
    pub fn new(path: &str, initial_balance: u64) -> Result<Self> {
        let db = Database::create(path)?;

        // Create the tables if they don't exist
        let write_txn = db.begin_write()?;
        {
            drop(write_txn.open_table(ACCOUNTS_TABLE)?);
            drop(write_txn.open_table(STATE_TABLE)?);
            drop(write_txn.open_table(COUNTER_TABLE)?);
            drop(write_txn.open_table(QUEUE_TABLE)?);
            drop(write_txn.open_table(TRANSACTIONS_TABLE)?);
        };
        write_txn.commit()?;

        Ok(Self {
            db: Arc::new(RwLock::new(db)),
            initial_balance,
        })
    }

    /// Get the balance of an account, creating it with initial balance if it
    /// doesn't exist.
    pub async fn get_or_create_balance(&self, account: &str) -> Result<u64> {
        let write_txn = self.db.write().await.begin_write()?;

        let balance = {
            let mut table = write_txn.open_table(ACCOUNTS_TABLE)?;

            if let Some(existing) = table.get(account)? {
                existing.value()
            } else {
                // New account - initialize with initial balance
                table.insert(account, self.initial_balance)?;
                self.initial_balance
            }
        };

        write_txn.commit()?;
        Ok(balance)
    }

    /// Transfer amount from one account to another.
    /// Creates accounts with initial balance if they don't exist.
    /// Returns error if sender has insufficient balance or if from == to.
    pub async fn transfer(&self, from: &str, to: &str, amount: u64) -> Result<(u64, u64)> {
        self.try_apply_transfers(once((from, to, amount))).await?;
        let read_txn = self
            .db
            .read()
            .await
            .begin_read()?
            .open_table(ACCOUNTS_TABLE)?;

        Ok((
            read_txn.get(from)?.unwrap().value(),
            read_txn.get(to)?.unwrap().value(),
        ))
    }

    pub async fn try_apply_transfers(
        &self,
        transfers: impl Iterator<Item = (&str, &str, u64)>,
    ) -> Result<()> {
        let write_txn = self.db.write().await.begin_write()?;
        {
            let mut table = write_txn.open_table(ACCOUNTS_TABLE)?;

            for (from, to, amount) in transfers {
                if from == to {
                    return Err(DbError::SelfTransfer {
                        account: from.to_owned(),
                    });
                }

                // Get or create 'from' account balance
                let from_balance = if let Some(existing) = table.get(&from)? {
                    existing.value()
                } else {
                    table.insert(&from, self.initial_balance)?;
                    self.initial_balance
                };

                // Check if sender has enough balance
                if from_balance < amount {
                    return Err(DbError::InsufficientBalance {
                        account: from.to_owned(),
                        balance: from_balance,
                        required: amount,
                    });
                }

                // Get or create 'to' account balance
                let to_balance = if let Some(existing) = table.get(&to)? {
                    existing.value()
                } else {
                    table.insert(&to, self.initial_balance)?;
                    self.initial_balance
                };

                // Perform the transfer
                let new_from_balance = from_balance - amount;
                let new_to_balance = to_balance + amount;

                table.insert(&from, new_from_balance)?;
                table.insert(&to, new_to_balance)?;
            }
        }

        write_txn.commit()?;
        Ok(())
    }

    /// List all accounts and their balances
    pub async fn list_accounts(&self) -> Result<Vec<(String, u64)>> {
        let read_txn = self.db.read().await.begin_read()?;
        let table = read_txn.open_table(ACCOUNTS_TABLE)?;

        let mut accounts = Vec::new();
        for entry in table.iter()? {
            let (key, value) = entry?;
            accounts.push((key.value().to_owned(), value.value()));
        }
        Ok(accounts)
    }

    /// Get the last message ID, returns None if not set (use root)
    pub async fn get_last_msg_id(&self) -> Result<Option<[u8; 32]>> {
        let read_txn = self.db.read().await.begin_read()?;
        let table = read_txn.open_table(STATE_TABLE)?;

        if let Some(value) = table.get(LAST_MSG_ID_KEY)? {
            let bytes = value.value();
            if bytes.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(bytes);
                return Ok(Some(arr));
            }
        }
        Ok(None)
    }

    /// Set the last message ID
    pub async fn set_last_msg_id(&self, msg_id: &[u8; 32]) -> Result<()> {
        let write_txn = self.db.write().await.begin_write()?;
        {
            let mut table = write_txn.open_table(STATE_TABLE)?;
            table.insert(LAST_MSG_ID_KEY, msg_id.as_slice())?;
        };
        write_txn.commit()?;
        Ok(())
    }

    /// Get the next block ID and increment the counter
    /// Returns (`new_block_id`, `parent_block_id`) where parent is 0 for
    /// genesis
    pub async fn next_block_id(&self) -> Result<(u64, u64)> {
        let write_txn = self.db.write().await.begin_write()?;

        let (block_id, parent_id) = {
            let mut table = write_txn.open_table(COUNTER_TABLE)?;
            let current = table.get(BLOCK_ID_KEY)?.map_or(0, |v| v.value());
            let next = current + 1;
            table.insert(BLOCK_ID_KEY, next)?;
            (next, current)
        };

        write_txn.commit()?;
        Ok((block_id, parent_id))
    }

    /// Get the next transaction index and increment the counter
    pub async fn next_tx_index(&self) -> Result<u64> {
        let write_txn = self.db.write().await.begin_write()?;

        let tx_index = {
            let mut table = write_txn.open_table(COUNTER_TABLE)?;
            let current = table.get(TX_INDEX_KEY)?.map_or(0, |v| v.value());
            let next = current + 1;
            table.insert(TX_INDEX_KEY, next)?;
            next
        };

        write_txn.commit()?;
        Ok(tx_index)
    }

    /// Add a pending transfer to the queue
    pub async fn queue_push(&self, tx_id: &str, data: &[u8]) -> Result<()> {
        let write_txn = self.db.write().await.begin_write()?;
        {
            let mut table = write_txn.open_table(QUEUE_TABLE)?;
            table.insert(tx_id, data)?;
        };
        write_txn.commit()?;
        Ok(())
    }

    /// Get all pending transfers from the queue and clear it
    pub async fn queue_drain(&self) -> Result<Vec<(String, Vec<u8>)>> {
        let write_txn = self.db.write().await.begin_write()?;

        let items = {
            let mut table = write_txn.open_table(QUEUE_TABLE)?;
            let mut items = Vec::new();
            for entry in table.iter()? {
                let (key, value) = entry?;
                items.push((key.value().to_owned(), value.value().to_vec()));
            }
            // Clear the queue
            for (key, _) in &items {
                table.remove(key.as_str())?;
            }
            items
        };

        write_txn.commit()?;
        Ok(items)
    }

    /// Check if queue is empty
    pub async fn queue_is_empty(&self) -> Result<bool> {
        let read_txn = self.db.read().await.begin_read()?;
        let table = read_txn.open_table(QUEUE_TABLE)?;
        Ok(table.is_empty()?)
    }

    /// Get queue length
    pub async fn queue_len(&self) -> Result<u64> {
        let read_txn = self.db.read().await.begin_read()?;
        let table = read_txn.open_table(QUEUE_TABLE)?;
        Ok(table.len()?)
    }

    /// Save a transaction to the transactions table
    pub async fn save_transaction(&self, tx_id: &str, data: &[u8]) -> Result<()> {
        let write_txn = self.db.write().await.begin_write()?;
        {
            let mut table = write_txn.open_table(TRANSACTIONS_TABLE)?;
            table.insert(tx_id, data)?;
        };
        write_txn.commit()?;
        Ok(())
    }

    pub async fn delete_transaction(&self, tx_id: &str) -> Result<()> {
        let write_txn = self.db.write().await.begin_write()?;
        {
            let mut table = write_txn.open_table(TRANSACTIONS_TABLE)?;
            table.remove(tx_id)?;
        };
        write_txn.commit()?;
        Ok(())
    }

    /// Get all raw transaction data from the database
    pub async fn get_all_transactions_raw(&self) -> Result<Vec<Vec<u8>>> {
        let read_txn = self.db.read().await.begin_read()?;
        let table = read_txn.open_table(TRANSACTIONS_TABLE)?;

        let mut transactions = Vec::new();
        for entry in table.iter()? {
            let (_key, value) = entry?;
            transactions.push(value.value().to_vec());
        }
        Ok(transactions)
    }

    #[must_use]
    pub const fn initial_balance(&self) -> u64 {
        self.initial_balance
    }
}
