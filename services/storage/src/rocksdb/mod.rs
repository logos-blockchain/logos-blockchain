mod handlers;
#[cfg(test)]
mod tests;

use std::{
    collections::{BTreeMap, HashMap},
    error,
    num::NonZeroUsize,
    ops::RangeInclusive,
    path::PathBuf,
    pin::Pin,
    sync::Arc,
};

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use futures::{Stream, StreamExt as _, stream};
pub(crate) use handlers::handle_request;
use lb_core::{header, header::HeaderId, mantle::TxHash};
use lb_cryptarchia_engine::Slot;
use lb_log_targets::storage;
use lb_utils::tokio::task::spawn_blocking;
use rocksdb::{DB, Direction, Error, IteratorMode, Options, WriteBatch};
use serde::{Deserialize, Serialize};

use crate::backend::{StorageBackend, StorageTransaction};

const IMMUTABLE_BLOCK_PREFIX: &str = "immutable_block/slot/";
const BLOCK_PARENT_PREFIX: &str = "block_parent/";
const BLOCK_EVENTS_PREFIX: &str = "block_events/";
const LOG_TARGET: &str = storage::rocksdb::CHAIN;

/// A stream of `HeaderId`s, used for scanning immutable header IDs. We return a
/// stream here to allow for efficient pagination of large ranges of immutable
/// blocks.
pub type HeaderIdStream =
    Pin<Box<dyn Stream<Item = Result<HeaderId, Box<dyn error::Error + Send + Sync>>> + Send>>;

#[derive(Debug, thiserror::Error)]
pub enum ChainError {
    #[error("RocksDB error: {0}")]
    RocksDbError(#[from] Error),
    #[error("Block header error: {0}")]
    BlockHeaderError(#[from] header::Error),
}

/// Rocks backend setting
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RocksBackendSettings {
    /// File path to the db file
    pub db_path: PathBuf,
    pub read_only: bool,
    pub column_family: Option<String>,
}

/// Rocks transaction type
// Do not use `TransactionDB` here, because rocksdb's `TransactionDB` does not
// support open by read-only mode. Thus, we cannot open the same db in two or
// more processes.
pub struct Transaction {
    rocks: Arc<DB>,
    #[expect(clippy::type_complexity, reason = "TODO: Address this at some point.")]
    executor: Box<dyn FnOnce(&DB) -> Result<Option<Bytes>, Error> + Send + Sync>,
}

impl Transaction {
    /// Execute a function over the transaction
    pub fn execute(self) -> Result<Option<Bytes>, Error> {
        (self.executor)(&self.rocks)
    }
}

impl StorageTransaction for Transaction {
    type Result = Result<Option<Bytes>, Error>;
    type Transaction = Self;
}

/// Rocks storage backend
#[derive(Clone)]
pub struct RocksBackend {
    rocks: Arc<DB>,
}

impl RocksBackend {
    pub fn txn(
        &self,
        executor: impl FnOnce(&DB) -> Result<Option<Bytes>, Error> + Send + Sync + 'static,
    ) -> Transaction {
        Transaction {
            rocks: Arc::clone(&self.rocks),
            executor: Box::new(executor),
        }
    }

    pub(crate) fn load_prefix_entries(
        &self,
        prefix: &[u8],
    ) -> Result<HashMap<Vec<u8>, Bytes>, Error> {
        let mut entries = HashMap::new();
        let iterator = self
            .rocks
            .iterator(IteratorMode::From(prefix, Direction::Forward));

        for item in iterator {
            let (key, value) = item?;
            if !key.starts_with(prefix) {
                break;
            }
            entries.insert(key.to_vec(), Bytes::from(value.to_vec()));
        }

        Ok(entries)
    }
    pub async fn get_block(&mut self, header_id: HeaderId) -> Result<Option<Bytes>, ChainError> {
        let header_id: [u8; 32] = header_id.into();
        let key = Bytes::copy_from_slice(&header_id);
        self.load(&key).await.map_err(Into::into)
    }

    pub async fn store_block_data(
        &mut self,
        header_id: HeaderId,
        parent_id: HeaderId,
        block: Bytes,
        events: Bytes,
        immutable_ids: BTreeMap<Slot, HeaderId>,
    ) -> Result<(), ChainError> {
        let header_bytes = <[u8; 32]>::from(header_id);
        let block_key = Bytes::copy_from_slice(&header_bytes);
        let parent_key = key_bytes(BLOCK_PARENT_PREFIX, header_bytes);
        let parent_value = Bytes::copy_from_slice(&<[u8; 32]>::from(parent_id));
        let events_key = key_bytes(BLOCK_EVENTS_PREFIX, header_bytes);

        let db_transaction = self.txn(move |db| {
            let mut batch = WriteBatch::default();
            batch.put(block_key, block);
            batch.put(parent_key, parent_value);
            batch.put(events_key, events);
            insert_immutable_block_ids(&mut batch, immutable_ids);
            db.write(batch)?;
            Ok(None)
        });
        drop(self.execute(db_transaction).await?);
        Ok(())
    }

    pub async fn remove_block(&mut self, header_id: HeaderId) -> Result<Option<Bytes>, ChainError> {
        let encoded_header_id: [u8; 32] = header_id.into();
        let block_key = Bytes::copy_from_slice(&encoded_header_id);
        let parent_key = key_bytes(BLOCK_PARENT_PREFIX, encoded_header_id);
        let events_key = key_bytes(BLOCK_EVENTS_PREFIX, encoded_header_id);

        // Load the block first so we can return it.
        let val = self.load(&block_key).await?;

        let db_transaction = self.txn(move |db| {
            let mut batch = WriteBatch::default();
            batch.delete(block_key);
            batch.delete(parent_key);
            batch.delete(events_key);
            db.write(batch)?;
            Ok(None)
        });
        drop(self.execute(db_transaction).await?);
        Ok(val)
    }

    pub async fn get_block_parent(
        &mut self,
        header_id: HeaderId,
    ) -> Result<Option<HeaderId>, ChainError> {
        let header_bytes: [u8; 32] = header_id.into();
        let key = key_bytes(BLOCK_PARENT_PREFIX, header_bytes);
        self.load(&key)
            .await?
            .map(|bytes| bytes.as_ref().try_into().map_err(Into::into))
            .transpose()
    }

    pub async fn get_block_events(
        &mut self,
        header_id: HeaderId,
    ) -> Result<Option<Bytes>, ChainError> {
        let header_bytes: [u8; 32] = header_id.into();
        let key = key_bytes(BLOCK_EVENTS_PREFIX, header_bytes);
        self.load(&key).await.map_err(Into::into)
    }

    pub async fn store_immutable_block_ids(
        &mut self,
        ids: BTreeMap<Slot, HeaderId>,
    ) -> Result<(), ChainError> {
        let db_transaction = self.txn(move |db| {
            let mut batch = WriteBatch::default();
            insert_immutable_block_ids(&mut batch, ids);
            db.write(batch)?;
            Ok(None)
        });
        drop(self.execute(db_transaction).await?);

        Ok(())
    }

    pub async fn get_immutable_block_id(
        &mut self,
        slot: Slot,
    ) -> Result<Option<HeaderId>, ChainError> {
        // use be_bytes to keep prefix ordering
        let key = key_bytes(IMMUTABLE_BLOCK_PREFIX, slot.to_be_bytes());
        self.load(&key)
            .await?
            .map(|bytes| bytes.as_ref().try_into().map_err(Into::into))
            .transpose()
    }

    pub async fn scan_immutable_block_ids(
        &mut self,
        slot_range: RangeInclusive<Slot>,
        limit: NonZeroUsize,
    ) -> Result<HeaderIdStream, ChainError> {
        // use be_bytes to keep prefix ordering
        let start_key = slot_range.start().to_be_bytes();
        let end_key = slot_range.end().to_be_bytes();
        let result = self
            .load_prefix(
                IMMUTABLE_BLOCK_PREFIX.as_ref(),
                Some(&start_key),
                Some(&end_key),
                Some(limit),
            )
            .await?;

        let mapped = result
            .into_iter()
            .map(|bytes| bytes.as_ref().try_into().map_err(Into::into));

        Ok(Box::pin(stream::iter(mapped)))
    }

    pub async fn scan_immutable_block_ids_reverse(
        &mut self,
        slot_range: RangeInclusive<Slot>,
        limit: NonZeroUsize,
    ) -> Result<HeaderIdStream, ChainError> {
        let start_key = slot_range.start().to_be_bytes();
        let end_key = slot_range.end().to_be_bytes();
        let result = self
            .load_prefix_reverse(
                IMMUTABLE_BLOCK_PREFIX.as_ref(),
                Some(&start_key),
                Some(&end_key),
                Some(limit),
            )
            .await?;

        let mapped = result
            .into_iter()
            .map(|bytes| bytes.as_ref().try_into().map_err(Into::into));

        Ok(Box::pin(stream::iter(mapped)))
    }

    pub async fn store_transactions(
        &mut self,
        transactions: HashMap<TxHash, Bytes>,
    ) -> Result<(), ChainError> {
        let batch_items: HashMap<Bytes, Bytes> = transactions
            .into_iter()
            .map(|(tx_hash, tx_bytes)| (tx_hash.into(), tx_bytes))
            .collect();

        self.bulk_store(batch_items).await.map_err(Into::into)
    }

    #[must_use]
    pub fn get_transactions(
        &self,
        tx_hashes: Vec<TxHash>,
    ) -> Pin<Box<dyn Stream<Item = Bytes> + Send>> {
        if tx_hashes.is_empty() {
            return Box::pin(stream::empty());
        }

        let stream = stream::iter(tx_hashes).filter_map({
            let backend = self.clone();
            move |tx_hash| {
                let mut backend = backend.clone();
                async move {
                    let key: Bytes = tx_hash.into();
                    match backend.load(&key).await {
                        Ok(Some(tx)) => Some(tx),
                        Ok(None) => {
                            tracing::debug!(target: LOG_TARGET, "Transaction not found: {tx_hash:?}");
                            None
                        }
                        Err(e) => {
                            tracing::error!(
                                target: LOG_TARGET,
                                "Database error loading transaction {tx_hash:?}: {e:?}",
                            );
                            None
                        }
                    }
                }
            }
        });

        Box::pin(stream)
    }

    pub async fn remove_transactions(&mut self, tx_hashes: &[TxHash]) -> Result<(), ChainError> {
        let keys: Vec<Bytes> = tx_hashes.iter().map(|&tx_hash| tx_hash.into()).collect();

        let db_transaction = self.txn(move |db| {
            let mut batch = WriteBatch::default();
            for key in keys {
                batch.delete(key);
            }
            db.write(batch)?;
            Ok(None)
        });

        drop(self.execute(db_transaction).await?);
        Ok(())
    }
}

impl core::fmt::Debug for RocksBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        format!("RocksBackend {{ rocks: {:?} }}", self.rocks).fmt(f)
    }
}

#[async_trait]
impl StorageBackend for RocksBackend {
    type Settings = RocksBackendSettings;
    type Error = Error;
    type Transaction = Transaction;

    fn new(config: Self::Settings) -> Result<Self, <Self as StorageBackend>::Error> {
        let RocksBackendSettings {
            db_path,
            read_only,
            column_family: cf,
        } = config;

        let db = match (read_only, cf) {
            (true, None) => {
                let mut opts = Options::default();
                opts.create_if_missing(false);
                DB::open_for_read_only(&opts, db_path, false)?
            }
            (true, Some(cf)) => {
                let mut opts = Options::default();
                opts.create_if_missing(false);
                DB::open_cf_for_read_only(&opts, db_path, [cf], false)?
            }
            (false, None) => {
                let mut opts = Options::default();
                opts.create_if_missing(true);
                opts.create_missing_column_families(true);
                DB::open(&opts, db_path)?
            }
            (false, Some(cf)) => {
                let mut opts = Options::default();
                opts.create_if_missing(true);
                opts.create_missing_column_families(true);
                DB::open_cf(&opts, db_path, [cf])?
            }
        };

        Ok(Self {
            rocks: Arc::new(db),
        })
    }

    async fn store(
        &mut self,
        key: Bytes,
        value: Bytes,
    ) -> Result<(), <Self as StorageBackend>::Error> {
        self.rocks.put(key, value)
    }

    async fn bulk_store<I>(&mut self, items: I) -> Result<(), <Self as StorageBackend>::Error>
    where
        I: IntoIterator<Item = (Bytes, Bytes)> + Send + 'static,
    {
        let rocks_db = Arc::clone(&self.rocks);

        // Use spawn_blocking to avoid blocking the async runtime during the bulk
        // operation
        spawn_blocking("logos/storage/rocksdb-bulk-store-blocking", move || {
            let mut batch = WriteBatch::default();
            let mut has_items = false;

            for (key, value) in items {
                batch.put(key, value);
                has_items = true;
            }

            if !has_items {
                return Ok(());
            }

            rocks_db.write(batch)
        })
        .await
        .expect("Failed to join the blocking task")
    }

    async fn load(&mut self, key: &[u8]) -> Result<Option<Bytes>, <Self as StorageBackend>::Error> {
        self.rocks.get(key).map(|opt| opt.map(Into::into))
    }

    async fn load_prefix(
        &mut self,
        prefix: &[u8],
        start_key: Option<&[u8]>,
        end_key: Option<&[u8]>,
        limit: Option<NonZeroUsize>,
    ) -> Result<Vec<Bytes>, <Self as StorageBackend>::Error> {
        let mut values = Vec::new();

        // NOTE: RocksDB has `prefix_iterator`, which sets `set_prefix_same_as_start`
        // to `true`. However, it works only if prefix_extractor is non-null for the
        // column family.
        // https://docs.rs/rocksdb/latest/rocksdb/struct.ReadOptions.html#method.set_prefix_same_as_start
        //
        // Since the column family is Optional in our
        // `RocksBackendSettings` and we don't configure any prefix extractor,
        // the `prefix_iterator` works like a regular iterator, which doesn't check
        // any upper bound.
        //
        // Thus, we use the regular iterator instead for clarity, and check the
        // upper bound manually.

        // Prepare the optional start and end keys by appending them to the prefix.
        let start_key = start_key.map(|k| key_bytes_raw(prefix, k));
        let end_key = end_key.map(|k| key_bytes_raw(prefix, k));

        // Create an iterator starting from the prefix or the start key if provided.
        let iter = self.rocks.iterator(IteratorMode::From(
            start_key.as_ref().map_or(prefix, |from| from.as_slice()),
            Direction::Forward,
        ));

        for item in iter {
            match item {
                Ok((key, value)) => {
                    // Since the iterator proceeds without an upper bound,
                    // we have to manually check for the prefix.
                    if !key.starts_with(prefix) {
                        break;
                    }

                    // Stop if we exceed the end key
                    if let Some(end_key) = &end_key {
                        // Lexicographical comparison
                        if key.as_ref() > end_key.as_slice() {
                            break;
                        }
                    }

                    values.push(Bytes::from(value.to_vec()));

                    // Stop if we reach the limit
                    if let Some(limit) = limit
                        && values.len() == limit.get()
                    {
                        break;
                    }
                }
                Err(e) => return Err(e), // Return the error if one occurs
            }
        }

        Ok(values)
    }

    async fn load_prefix_reverse(
        &mut self,
        prefix: &[u8],
        start_key: Option<&[u8]>,
        end_key: Option<&[u8]>,
        limit: Option<NonZeroUsize>,
    ) -> Result<Vec<Bytes>, <Self as StorageBackend>::Error> {
        let mut values = Vec::new();

        let start_key = start_key.map(|k| key_bytes_raw(prefix, k));
        let end_key = end_key.map(|k| key_bytes_raw(prefix, k));

        let iter = self.rocks.iterator(IteratorMode::From(
            end_key.as_ref().map_or(prefix, |to| to.as_slice()),
            Direction::Reverse,
        ));

        for item in iter {
            match item {
                Ok((key, value)) => {
                    if !key.starts_with(prefix) {
                        break;
                    }

                    if let Some(start_key) = &start_key
                        && key.as_ref() < start_key.as_slice()
                    {
                        break;
                    }

                    values.push(Bytes::from(value.to_vec()));

                    if let Some(limit) = limit
                        && values.len() == limit.get()
                    {
                        break;
                    }
                }
                Err(e) => return Err(e),
            }
        }

        Ok(values)
    }

    async fn remove(
        &mut self,
        key: &[u8],
    ) -> Result<Option<Bytes>, <Self as StorageBackend>::Error> {
        let val = self.load(key).await?;
        if val.is_some() {
            self.rocks.delete(key).map(|()| val)
        } else {
            Ok(None)
        }
    }

    async fn execute(
        &mut self,
        transaction: Self::Transaction,
    ) -> Result<<Self::Transaction as StorageTransaction>::Result, <Self as StorageBackend>::Error>
    {
        Ok(transaction.execute())
    }
}

fn insert_immutable_block_ids(batch: &mut WriteBatch, ids: BTreeMap<Slot, HeaderId>) {
    for (slot, header_id) in ids {
        // Use big-endian bytes to keep prefix ordering.
        let key = key_bytes(IMMUTABLE_BLOCK_PREFIX, slot.to_be_bytes());
        let header_id = <[u8; 32]>::from(header_id);
        batch.put(key, Bytes::copy_from_slice(&header_id));
    }
}

fn key_bytes(prefix: &str, id: impl AsRef<[u8]>) -> Bytes {
    let mut buffer = BytesMut::new();

    buffer.extend_from_slice(prefix.as_bytes());
    buffer.extend_from_slice(id.as_ref());

    buffer.freeze()
}

fn key_bytes_raw(prefix: &[u8], suffix: impl AsRef<[u8]>) -> Vec<u8> {
    prefix
        .iter()
        .chain(suffix.as_ref().iter())
        .copied()
        .collect()
}
