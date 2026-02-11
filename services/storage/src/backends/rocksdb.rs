use std::{num::NonZeroUsize, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use bytes::Bytes;
use rocksdb::{DB, Direction, Error, IteratorMode, Options};
use serde::{Deserialize, Serialize};

use super::{StorageBackend, StorageTransaction};

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
        tokio::task::spawn_blocking(move || {
            let mut batch = rocksdb::WriteBatch::default();
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
        let start_key =
            start_key.map(|k| prefix.iter().chain(k.iter()).copied().collect::<Vec<_>>());
        let end_key = end_key.map(|k| prefix.iter().chain(k.iter()).copied().collect::<Vec<_>>());

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

#[cfg(test)]
mod test {
    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    async fn test_store_load_remove() -> Result<(), <RocksBackend as StorageBackend>::Error> {
        let temp_path = TempDir::new().unwrap();
        let sled_settings = RocksBackendSettings {
            db_path: temp_path.path().to_path_buf(),
            read_only: false,
            column_family: None,
        };
        let key = "foo";
        let value = "bar";

        let mut db: RocksBackend = RocksBackend::new(sled_settings)?;
        db.store(key.as_bytes().into(), value.as_bytes().into())
            .await?;
        let load_value = db.load(key.as_bytes()).await?;
        assert_eq!(load_value, Some(value.as_bytes().into()));
        let removed_value = db.remove(key.as_bytes()).await?;
        assert_eq!(removed_value, Some(value.as_bytes().into()));

        Ok(())
    }

    #[tokio::test]
    async fn test_load_prefix() {
        let mut backend = RocksBackend::new(RocksBackendSettings {
            db_path: TempDir::new().unwrap().path().to_path_buf(),
            read_only: false,
            column_family: None,
        })
        .unwrap();

        let prefix = b"foo/";

        // No data yet in the backend
        assert!(
            backend
                .load_prefix(prefix, None, None, None)
                .await
                .unwrap()
                .is_empty()
        );

        // No data with the prefix
        backend.store("boo/0".into(), "boo0".into()).await.unwrap();
        backend.store("zoo/0".into(), "zoo0".into()).await.unwrap();
        assert!(
            backend
                .load_prefix(prefix, None, None, None)
                .await
                .unwrap()
                .is_empty()
        );

        // Two data with the prefix
        // (Inserting in mixed order to test the sorted scan).
        backend.store("foo/7".into(), "foo7".into()).await.unwrap();
        backend.store("foo/0".into(), "foo0".into()).await.unwrap();
        assert_eq!(
            backend.load_prefix(prefix, None, None, None).await.unwrap(),
            vec![Bytes::from("foo0"), Bytes::from("foo7")]
        );
    }

    #[tokio::test]
    async fn test_load_prefix_range_limit() {
        let mut backend = RocksBackend::new(RocksBackendSettings {
            db_path: TempDir::new().unwrap().path().to_path_buf(),
            read_only: false,
            column_family: None,
        })
        .unwrap();
        backend.store("boo/0".into(), "boo0".into()).await.unwrap();
        backend.store("foo/0".into(), "foo0".into()).await.unwrap();
        backend.store("foo/1".into(), "foo1".into()).await.unwrap();
        backend.store("foo/3".into(), "foo3".into()).await.unwrap();
        backend.store("foo/4".into(), "foo4".into()).await.unwrap();
        backend.store("foo/9".into(), "foo9".into()).await.unwrap();
        backend.store("zoo/4".into(), "zoo4".into()).await.unwrap();

        // with start_key and end_key that exist
        assert_eq!(
            backend
                .load_prefix(b"foo/", Some(b"1"), Some(b"9"), None)
                .await
                .unwrap(),
            vec![
                Bytes::from("foo1"),
                Bytes::from("foo3"),
                Bytes::from("foo4"),
                Bytes::from("foo9")
            ]
        );

        // with start_key that doesn't exist
        assert_eq!(
            backend
                .load_prefix(b"foo/", Some(b"2"), Some(b"9"), None)
                .await
                .unwrap(),
            vec![
                Bytes::from("foo3"),
                Bytes::from("foo4"),
                Bytes::from("foo9")
            ]
        );

        // with end_key that doesn't exist
        assert_eq!(
            backend
                .load_prefix(b"foo/", Some(b"1"), Some(b"8"), None)
                .await
                .unwrap(),
            vec![
                Bytes::from("foo1"),
                Bytes::from("foo3"),
                Bytes::from("foo4"),
            ]
        );

        // with limit
        assert_eq!(
            backend
                .load_prefix(
                    b"foo/",
                    Some(b"1"),
                    Some(b"9"),
                    Some(NonZeroUsize::new(3).unwrap())
                )
                .await
                .unwrap(),
            vec![
                Bytes::from("foo1"),
                Bytes::from("foo3"),
                Bytes::from("foo4"),
            ]
        );
    }

    #[tokio::test]
    async fn test_transaction() -> Result<(), <RocksBackend as StorageBackend>::Error> {
        let temp_path = TempDir::new().unwrap();

        let sled_settings = RocksBackendSettings {
            db_path: temp_path.path().to_path_buf(),
            read_only: false,
            column_family: None,
        };

        let mut db: RocksBackend = RocksBackend::new(sled_settings)?;
        let txn = db.txn(|db| {
            let key = "foo";
            let value = "bar";
            db.put(key, value)?;
            let result = db.get(key)?;
            db.delete(key)?;
            Ok(result.map(Into::into))
        });
        let result = db.execute(txn).await??;
        assert_eq!(result, Some(b"bar".as_ref().into()));

        Ok(())
    }

    #[tokio::test]
    async fn test_multi_readers_single_writer()
    -> Result<(), <RocksBackend as StorageBackend>::Error> {
        use tokio::sync::mpsc::channel;

        let temp_path = TempDir::new().unwrap();
        let path = temp_path.path().to_path_buf();
        let sled_settings = RocksBackendSettings {
            db_path: temp_path.path().to_path_buf(),
            read_only: false,
            column_family: None,
        };
        let key = "foo";
        let value = "bar";

        let mut db: RocksBackend = RocksBackend::new(sled_settings)?;

        let (tx, mut rx) = channel(5);
        // now let us spawn a few readers
        for _ in 0..5 {
            let p = path.clone();
            let tx = tx.clone();
            std::thread::spawn(move || {
                tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(async move {
                        let sled_settings = RocksBackendSettings {
                            db_path: p,
                            read_only: true,
                            column_family: None,
                        };
                        let key = "foo";

                        let mut db: RocksBackend = RocksBackend::new(sled_settings).unwrap();

                        while db.load(key.as_bytes()).await.unwrap().is_none() {
                            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                        }

                        tx.send(()).await.unwrap();
                    });
            });
        }

        db.store(key.as_bytes().into(), value.as_bytes().into())
            .await?;

        let mut recvs = 0;
        loop {
            if rx.recv().await.is_some() {
                recvs += 1;
                if recvs == 5 {
                    break;
                }
            }
        }
        Ok(())
    }
}
