//! Opening a store from settings, so a service can take the type of its store
//! as a parameter and open it itself.

use std::path::PathBuf;

use crate::{KvEntry, KvStore, MemKv, WriteBatch};

/// Where a store opened by [`OpenKvStore::open`] keeps its data, and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KvStoreSettings {
    /// The directory of a store on disk. A store kept in memory ignores it.
    pub path: PathBuf,
    /// Syncs every batch to disk before it returns. A store kept in memory
    /// ignores it.
    pub sync_writes: bool,
}

/// A store a service opens from its settings.
pub trait OpenKvStore: KvStore + Sized + 'static {
    fn open(settings: &KvStoreSettings) -> Result<Self, Self::Error>;
}

/// A new, empty store at every opening: nothing survives a restart.
impl OpenKvStore for MemKv {
    fn open(_settings: &KvStoreSettings) -> Result<Self, Self::Error> {
        Ok(Self::new())
    }
}

#[cfg(feature = "rocksdb")]
impl OpenKvStore for crate::RocksKv {
    fn open(settings: &KvStoreSettings) -> Result<Self, Self::Error> {
        Self::open_with(
            &settings.path,
            crate::RocksKvOptions {
                sync_writes: settings.sync_writes,
                ..crate::RocksKvOptions::default()
            },
        )
    }
}

/// The error of every operation of [`DetachedKv`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the store was not attached: the data was decoded without it")]
pub struct Detached;

/// A store standing for the one data was decoded without, until it is attached
/// to it: every operation fails.
#[derive(Clone, Copy, Debug, Default)]
pub struct DetachedKv;

impl KvStore for DetachedKv {
    type Error = Detached;

    fn get(&self, _key: &[u8]) -> Result<Option<crate::Bytes>, Self::Error> {
        Err(Detached)
    }

    fn get_prev(&self, _prefix: &[u8], _key: &[u8]) -> Result<Option<KvEntry>, Self::Error> {
        Err(Detached)
    }

    fn scan_prefix(&self, _prefix: &[u8]) -> Result<Vec<KvEntry>, Self::Error> {
        Err(Detached)
    }

    fn write_batch(&self, _batch: WriteBatch) -> Result<(), Self::Error> {
        Err(Detached)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detached_store_fails_every_operation() {
        let store = DetachedKv;
        assert_eq!(store.get(b"k"), Err(Detached));
        assert_eq!(store.get_prev(b"", b"k"), Err(Detached));
        assert_eq!(store.scan_prefix(b""), Err(Detached));
        assert_eq!(store.write_batch(WriteBatch::new()), Err(Detached));
    }

    #[test]
    fn test_memory_store_opens_empty() {
        let settings = KvStoreSettings {
            path: PathBuf::from("ignored"),
            sync_writes: false,
        };
        let store = MemKv::open(&settings).expect("a store in memory opens");
        assert!(store.is_empty());
    }

    #[cfg(feature = "rocksdb")]
    #[test]
    fn test_rocks_store_opens_from_settings() {
        let directory = tempfile::tempdir().expect("a temporary directory is created");
        let settings = KvStoreSettings {
            path: directory.path().join("store"),
            sync_writes: true,
        };
        let mut batch = WriteBatch::new();
        batch.put(&b"k"[..], &b"v"[..]);
        <crate::RocksKv as OpenKvStore>::open(&settings)
            .expect("the database opens")
            .write_batch(batch)
            .expect("the batch is written");
        let reopened =
            <crate::RocksKv as OpenKvStore>::open(&settings).expect("the database reopens");
        assert_eq!(
            reopened.get(b"k").expect("the store is read"),
            Some(crate::Bytes::from_static(b"v"))
        );
    }
}
