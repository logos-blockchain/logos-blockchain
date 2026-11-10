//! A [`KvStore`] on a `RocksDB` database of its own.
//!
//! The store owns its database and uses the default column family, ordered
//! bytewise like every [`KvStore`]. Each [`WriteBatch`] becomes one `RocksDB`
//! batch, so it is applied atomically, and its writes take increasing sequence
//! numbers, so a later write of a key wins over an earlier one, including a
//! [`WriteOp::DeletePrefix`] followed by a put in the same batch.

use std::{path::Path, sync::Arc};

pub use rocksdb::Error as RocksError;
use rocksdb::{BlockBasedOptions, DB, Direction, IteratorMode, Options, ReadOptions, WriteOptions};

use crate::{Bytes, KvEntry, KvStore, WriteBatch, WriteOp, prefix_end};

/// Tuning of a [`RocksKv`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RocksKvOptions {
    /// Syncs the write-ahead log at every batch. Without it a batch survives a
    /// crash of the process but may be lost, with the batches after it, on a
    /// crash of the machine; batches are never torn or reordered either way.
    pub sync_writes: bool,
    /// Bits per key of the bloom filter on whole keys, which saves the disk
    /// reads of `get` on absent keys. `None` disables the filter.
    pub bloom_bits_per_key: Option<f64>,
}

impl Default for RocksKvOptions {
    fn default() -> Self {
        Self {
            sync_writes: false,
            bloom_bits_per_key: Some(10.0),
        }
    }
}

/// A [`KvStore`] on a `RocksDB` database.
///
/// Reads and writes run on the calling thread: the ledger applies blocks
/// synchronously, outside of the async runtime's worker threads.
pub struct RocksKv {
    db: Arc<DB>,
    sync_writes: bool,
}

impl RocksKv {
    /// Opens the database at `path`, creating it if missing, with the default
    /// options.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RocksError> {
        Self::open_with(path, RocksKvOptions::default())
    }

    /// Opens the database at `path`, creating it if missing.
    pub fn open_with(path: impl AsRef<Path>, options: RocksKvOptions) -> Result<Self, RocksError> {
        let mut db_options = Options::default();
        db_options.create_if_missing(true);
        if let Some(bits) = options.bloom_bits_per_key {
            let mut table = BlockBasedOptions::default();
            table.set_bloom_filter(bits, false);
            table.set_whole_key_filtering(true);
            db_options.set_block_based_table_factory(&table);
        }
        Ok(Self {
            db: Arc::new(DB::open(&db_options, path)?),
            sync_writes: options.sync_writes,
        })
    }

    fn entry(key: &[u8], value: &[u8]) -> KvEntry {
        KvEntry {
            key: Bytes::copy_from_slice(key),
            value: Bytes::copy_from_slice(value),
        }
    }

    /// The greatest stored key at or above `prefix`, for a prefix whose keys
    /// have no upper bound.
    fn last_key_from(&self, prefix: &[u8]) -> Result<Option<Vec<u8>>, RocksError> {
        let mut iterator = self.db.raw_iterator();
        iterator.seek_to_last();
        iterator.status()?;
        Ok(iterator
            .key()
            .filter(|key| *key >= prefix)
            .map(<[u8]>::to_vec))
    }
}

impl std::fmt::Debug for RocksKv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RocksKv")
            .field("path", &self.db.path())
            .field("sync_writes", &self.sync_writes)
            .finish_non_exhaustive()
    }
}

impl KvStore for RocksKv {
    type Error = RocksError;

    fn get(&self, key: &[u8]) -> Result<Option<Bytes>, Self::Error> {
        Ok(self
            .db
            .get_pinned(key)?
            .map(|value| Bytes::copy_from_slice(&value)))
    }

    fn get_prev(&self, prefix: &[u8], key: &[u8]) -> Result<Option<KvEntry>, Self::Error> {
        // every key starting with `prefix` is at least `prefix`
        if key <= prefix {
            return Ok(None);
        }
        // a bound past the prefix is clamped to its end
        let upper = match prefix_end(prefix) {
            Some(end) if end.as_slice() < key => end,
            _ => key.to_vec(),
        };
        let mut read = ReadOptions::default();
        read.set_iterate_lower_bound(prefix);
        read.set_iterate_upper_bound(upper);
        let mut iterator = self.db.raw_iterator_opt(read);
        iterator.seek_to_last();
        iterator.status()?;
        Ok(iterator
            .item()
            .filter(|(found, _)| found.starts_with(prefix))
            .map(|(key, value)| Self::entry(key, value)))
    }

    fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<KvEntry>, Self::Error> {
        let mut read = ReadOptions::default();
        read.set_iterate_lower_bound(prefix);
        if let Some(end) = prefix_end(prefix) {
            read.set_iterate_upper_bound(end);
        }
        self.db
            .iterator_opt(IteratorMode::From(prefix, Direction::Forward), read)
            .map(|item| item.map(|(key, value)| Self::entry(&key, &value)))
            .collect()
    }

    /// A prefix made of `0xff` bytes only, or empty, has no end key, so the
    /// range it deletes ends past the greatest key it covers: the greatest
    /// stored, read before the batch is written, or put earlier in the batch.
    /// Such a delete is not atomic with the writes of another thread.
    fn write_batch(&self, batch: WriteBatch) -> Result<(), Self::Error> {
        if batch.is_empty() {
            return Ok(());
        }
        let mut rocks_batch = rocksdb::WriteBatch::default();
        let mut greatest_put: Option<Bytes> = None;
        for op in batch.into_ops() {
            match op {
                WriteOp::Put(KvEntry { key, value }) => {
                    if greatest_put.as_ref().is_none_or(|greatest| key > *greatest) {
                        greatest_put = Some(key.clone());
                    }
                    rocks_batch.put(key, value);
                }
                WriteOp::Delete { key } => rocks_batch.delete(key),
                WriteOp::DeletePrefix { prefix } => {
                    if let Some(end) = prefix_end(&prefix) {
                        rocks_batch.delete_range(prefix.to_vec(), end);
                        continue;
                    }
                    let put = greatest_put
                        .as_ref()
                        .filter(|key| key.as_ref() >= prefix.as_ref())
                        .map(|key| key.to_vec());
                    let Some(mut last) = self.last_key_from(&prefix)?.max(put) else {
                        continue;
                    };
                    // the range end is exclusive
                    last.push(0);
                    rocks_batch.delete_range(prefix.to_vec(), last);
                }
            }
        }
        let mut write = WriteOptions::default();
        write.set_sync(self.sync_writes);
        self.db.write_opt(rocks_batch, &write)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conformance;

    #[test]
    fn test_rocks_kv_conformance() {
        let directory = tempfile::tempdir().expect("a temporary directory is created");
        let count = std::sync::atomic::AtomicUsize::new(0);
        conformance::run(|| {
            let index = count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            RocksKv::open(directory.path().join(index.to_string())).expect("the database opens")
        });
    }

    #[test]
    fn test_rocks_kv_conformance_without_bloom_filter() {
        let directory = tempfile::tempdir().expect("a temporary directory is created");
        let count = std::sync::atomic::AtomicUsize::new(0);
        let options = RocksKvOptions {
            sync_writes: true,
            bloom_bits_per_key: None,
        };
        conformance::run(|| {
            let index = count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            RocksKv::open_with(directory.path().join(index.to_string()), options)
                .expect("the database opens")
        });
    }

    #[test]
    fn test_writes_survive_reopening() {
        let directory = tempfile::tempdir().expect("a temporary directory is created");
        let mut batch = WriteBatch::new();
        batch
            .put(&b"a/1"[..], &b"x"[..])
            .put(&b"a/2"[..], &b"y"[..])
            .put(&b"b/1"[..], &b"z"[..]);
        let store = RocksKv::open(directory.path()).expect("the database opens");
        store.write_batch(batch).expect("the batch is written");
        let mut batch = WriteBatch::new();
        batch.delete_prefix(&b"a/"[..]).put(&b"a/3"[..], &b"w"[..]);
        store.write_batch(batch).expect("the batch is written");
        drop(store);

        let store = RocksKv::open(directory.path()).expect("the database reopens");
        assert_eq!(
            store.scan_prefix(b"").expect("the store is read"),
            vec![RocksKv::entry(b"a/3", b"w"), RocksKv::entry(b"b/1", b"z"),]
        );
    }

    #[test]
    fn test_unbounded_prefix_deletes_keys_put_in_the_same_batch() {
        let directory = tempfile::tempdir().expect("a temporary directory is created");
        let store = RocksKv::open(directory.path()).expect("the database opens");
        let mut batch = WriteBatch::new();
        batch
            .put(&[0xff, 0x01][..], &b"stored"[..])
            .put(&[0x01][..], &b"kept"[..]);
        store.write_batch(batch).expect("the batch is written");
        let mut batch = WriteBatch::new();
        batch
            .put(&[0xff, 0xff, 0xff, 0x07][..], &b"put"[..])
            .delete_prefix(&[0xff][..])
            .put(&[0xff, 0x02][..], &b"after"[..]);
        store.write_batch(batch).expect("the batch is written");
        assert_eq!(
            store.scan_prefix(b"").expect("the store is read"),
            vec![
                RocksKv::entry(&[0x01], b"kept"),
                RocksKv::entry(&[0xff, 0x02], b"after"),
            ]
        );

        let mut batch = WriteBatch::new();
        batch.put(&[0x09][..], &b"put"[..]).delete_prefix(&b""[..]);
        store.write_batch(batch).expect("the batch is written");
        assert_eq!(store.scan_prefix(b"").expect("the store is read"), vec![]);
    }
}
