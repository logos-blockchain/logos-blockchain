//! A synchronous, ordered key value store abstraction for the state trees.
//!
//! The ledger applies blocks synchronously, so the trees it holds read and
//! write their storage through [`KvStore`] rather than through the async
//! storage service. Keys are ordered bytewise: [`KvStore::get_prev`] finds the
//! greatest key below a bound, and [`WriteOp::DeletePrefix`] removes a whole
//! key range at once.
//!
//! [`MemKv`] keeps everything in a [`BTreeMap`], for tests and benches.
//! [`Prefixed`] namespaces the keys of a store, so several trees share one.
//! [`ErasedKv`] and [`DynKvStore`] erase the backend type behind a trait
//! object.

use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    ops::Bound,
    sync::{
        Arc, PoisonError, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

pub use bytes::Bytes;

#[cfg(any(test, feature = "testing"))]
pub mod conformance;

/// A stored key and its value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KvEntry {
    pub key: Bytes,
    pub value: Bytes,
}

/// One write of a [`WriteBatch`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteOp {
    Put(KvEntry),
    Delete {
        key: Bytes,
    },
    /// Deletes every key starting with `prefix`.
    DeletePrefix {
        prefix: Bytes,
    },
}

/// Writes applied atomically and in order: a later write of a key wins over an
/// earlier one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WriteBatch {
    ops: Vec<WriteOp>,
}

impl WriteBatch {
    #[must_use]
    pub const fn new() -> Self {
        Self { ops: Vec::new() }
    }

    pub fn put(&mut self, key: impl Into<Bytes>, value: impl Into<Bytes>) -> &mut Self {
        self.ops.push(WriteOp::Put(KvEntry {
            key: key.into(),
            value: value.into(),
        }));
        self
    }

    pub fn delete(&mut self, key: impl Into<Bytes>) -> &mut Self {
        self.ops.push(WriteOp::Delete { key: key.into() });
        self
    }

    pub fn delete_prefix(&mut self, prefix: impl Into<Bytes>) -> &mut Self {
        self.ops.push(WriteOp::DeletePrefix {
            prefix: prefix.into(),
        });
        self
    }

    /// Appends the writes of `other`, which then apply after those of `self`.
    pub fn append(&mut self, other: Self) -> &mut Self {
        self.ops.extend(other.ops);
        self
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.ops.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    #[must_use]
    pub fn ops(&self) -> &[WriteOp] {
        &self.ops
    }

    #[must_use]
    pub fn into_ops(self) -> Vec<WriteOp> {
        self.ops
    }
}

impl FromIterator<WriteOp> for WriteBatch {
    fn from_iter<I: IntoIterator<Item = WriteOp>>(iter: I) -> Self {
        Self {
            ops: iter.into_iter().collect(),
        }
    }
}

/// A synchronous key value store whose keys are ordered bytewise.
pub trait KvStore: Send + Sync {
    type Error: Error + Send + Sync + 'static;

    fn get(&self, key: &[u8]) -> Result<Option<Bytes>, Self::Error>;

    /// Returns the entry of the greatest key lower than `key` among the keys
    /// starting with `prefix`.
    fn get_prev(&self, prefix: &[u8], key: &[u8]) -> Result<Option<KvEntry>, Self::Error>;

    /// Returns every entry whose key starts with `prefix`, in key order.
    fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<KvEntry>, Self::Error>;

    /// Applies the writes of `batch` atomically: all of them or none.
    fn write_batch(&self, batch: WriteBatch) -> Result<(), Self::Error>;
}

impl<S: KvStore + ?Sized> KvStore for Arc<S> {
    type Error = S::Error;

    fn get(&self, key: &[u8]) -> Result<Option<Bytes>, Self::Error> {
        (**self).get(key)
    }

    fn get_prev(&self, prefix: &[u8], key: &[u8]) -> Result<Option<KvEntry>, Self::Error> {
        (**self).get_prev(prefix, key)
    }

    fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<KvEntry>, Self::Error> {
        (**self).scan_prefix(prefix)
    }

    fn write_batch(&self, batch: WriteBatch) -> Result<(), Self::Error> {
        (**self).write_batch(batch)
    }
}

/// The first key past every key starting with `prefix`, or `None` if there is
/// none (an empty prefix or one made of `0xff` bytes only).
#[must_use]
pub fn prefix_end(prefix: &[u8]) -> Option<Vec<u8>> {
    let last = prefix.iter().rposition(|byte| *byte != u8::MAX)?;
    let mut end = prefix[..=last].to_vec();
    end[last] += 1;
    Some(end)
}

/// The error of a type erased store.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct StoreError(Box<dyn Error + Send + Sync + 'static>);

impl StoreError {
    pub fn new(error: impl Error + Send + Sync + 'static) -> Self {
        Self(Box::new(error))
    }
}

/// A store whose backend type is erased.
pub type DynKvStore = Arc<dyn KvStore<Error = StoreError>>;

/// Wraps a store to report its errors as [`StoreError`], so it can be used as
/// a [`DynKvStore`].
pub struct ErasedKv<S>(pub S);

impl<S: KvStore> ErasedKv<S> {
    pub fn into_dyn(store: S) -> DynKvStore
    where
        S: 'static,
    {
        Arc::new(Self(store))
    }
}

impl<S: KvStore> KvStore for ErasedKv<S> {
    type Error = StoreError;

    fn get(&self, key: &[u8]) -> Result<Option<Bytes>, Self::Error> {
        self.0.get(key).map_err(StoreError::new)
    }

    fn get_prev(&self, prefix: &[u8], key: &[u8]) -> Result<Option<KvEntry>, Self::Error> {
        self.0.get_prev(prefix, key).map_err(StoreError::new)
    }

    fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<KvEntry>, Self::Error> {
        self.0.scan_prefix(prefix).map_err(StoreError::new)
    }

    fn write_batch(&self, batch: WriteBatch) -> Result<(), Self::Error> {
        self.0.write_batch(batch).map_err(StoreError::new)
    }
}

/// The keys of a store under a fixed namespace: every key is stored as
/// `namespace ‖ key`, and the keys outside the namespace are invisible.
#[derive(Clone)]
pub struct Prefixed<S> {
    store: S,
    namespace: Bytes,
}

impl<S> Prefixed<S> {
    pub fn new(store: S, namespace: impl Into<Bytes>) -> Self {
        Self {
            store,
            namespace: namespace.into(),
        }
    }

    fn key(&self, key: &[u8]) -> Bytes {
        let mut full = Vec::with_capacity(self.namespace.len() + key.len());
        full.extend_from_slice(&self.namespace);
        full.extend_from_slice(key);
        full.into()
    }

    /// The store holding the namespace.
    pub const fn inner(&self) -> &S {
        &self.store
    }

    /// The writes of `batch`, made within the namespace, as writes of the
    /// inner store: to apply them atomically with writes of other namespaces.
    #[must_use]
    pub fn namespace_batch(&self, batch: WriteBatch) -> WriteBatch {
        batch
            .into_ops()
            .into_iter()
            .map(|op| match op {
                WriteOp::Put(entry) => WriteOp::Put(KvEntry {
                    key: self.key(&entry.key),
                    value: entry.value,
                }),
                WriteOp::Delete { key } => WriteOp::Delete {
                    key: self.key(&key),
                },
                WriteOp::DeletePrefix { prefix } => WriteOp::DeletePrefix {
                    prefix: self.key(&prefix),
                },
            })
            .collect()
    }

    fn strip(&self, entry: KvEntry) -> KvEntry {
        KvEntry {
            key: entry.key.slice(self.namespace.len()..),
            value: entry.value,
        }
    }
}

impl<S: fmt::Debug> fmt::Debug for Prefixed<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Prefixed")
            .field("store", &self.store)
            .field("namespace", &self.namespace)
            .finish()
    }
}

impl<S: KvStore> KvStore for Prefixed<S> {
    type Error = S::Error;

    fn get(&self, key: &[u8]) -> Result<Option<Bytes>, Self::Error> {
        self.store.get(&self.key(key))
    }

    fn get_prev(&self, prefix: &[u8], key: &[u8]) -> Result<Option<KvEntry>, Self::Error> {
        Ok(self
            .store
            .get_prev(&self.key(prefix), &self.key(key))?
            .map(|entry| self.strip(entry)))
    }

    fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<KvEntry>, Self::Error> {
        Ok(self
            .store
            .scan_prefix(&self.key(prefix))?
            .into_iter()
            .map(|entry| self.strip(entry))
            .collect())
    }

    fn write_batch(&self, batch: WriteBatch) -> Result<(), Self::Error> {
        self.store.write_batch(self.namespace_batch(batch))
    }
}

/// Counts of the operations served by a [`MemKv`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemKvStats {
    /// `get`, `get_prev` and `scan_prefix` calls.
    pub reads: u64,
    /// Writes of the applied batches.
    pub writes: u64,
}

/// An in-memory [`KvStore`], for tests and benches.
#[derive(Default)]
pub struct MemKv {
    entries: RwLock<BTreeMap<Bytes, Bytes>>,
    reads: AtomicU64,
    writes: AtomicU64,
}

impl fmt::Debug for MemKv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemKv")
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

/// [`MemKv`] never fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Infallible {}

impl MemKv {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// A copy of every entry, in key order.
    #[must_use]
    pub fn entries(&self) -> Vec<KvEntry> {
        self.entries
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(key, value)| KvEntry {
                key: key.clone(),
                value: value.clone(),
            })
            .collect()
    }

    #[must_use]
    pub fn stats(&self) -> MemKvStats {
        MemKvStats {
            reads: self.reads.load(Ordering::Relaxed),
            writes: self.writes.load(Ordering::Relaxed),
        }
    }

    fn count_read(&self) {
        self.reads.fetch_add(1, Ordering::Relaxed);
    }

    fn range_bounds(prefix: &[u8]) -> (Bound<Bytes>, Bound<Bytes>) {
        (
            Bound::Included(Bytes::copy_from_slice(prefix)),
            prefix_end(prefix).map_or(Bound::Unbounded, |end| Bound::Excluded(end.into())),
        )
    }
}

impl KvStore for MemKv {
    type Error = Infallible;

    fn get(&self, key: &[u8]) -> Result<Option<Bytes>, Self::Error> {
        self.count_read();
        Ok(self
            .entries
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .cloned())
    }

    fn get_prev(&self, prefix: &[u8], key: &[u8]) -> Result<Option<KvEntry>, Self::Error> {
        self.count_read();
        // every key starting with `prefix` is at least `prefix`
        if key <= prefix {
            return Ok(None);
        }
        // a bound past the prefix is clamped to its end
        let end = match prefix_end(prefix) {
            Some(end) if end.as_slice() < key => end,
            _ => key.to_vec(),
        };
        Ok(self
            .entries
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .range::<[u8], _>((Bound::Included(prefix), Bound::Excluded(end.as_slice())))
            .next_back()
            .filter(|(found, _)| found.starts_with(prefix))
            .map(|(key, value)| KvEntry {
                key: key.clone(),
                value: value.clone(),
            }))
    }

    fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<KvEntry>, Self::Error> {
        self.count_read();
        Ok(self
            .entries
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .range(Self::range_bounds(prefix))
            .map(|(key, value)| KvEntry {
                key: key.clone(),
                value: value.clone(),
            })
            .collect())
    }

    fn write_batch(&self, batch: WriteBatch) -> Result<(), Self::Error> {
        let mut entries = self.entries.write().unwrap_or_else(PoisonError::into_inner);
        self.writes.fetch_add(batch.len() as u64, Ordering::Relaxed);
        for op in batch.into_ops() {
            match op {
                WriteOp::Put(entry) => {
                    entries.insert(entry.key, entry.value);
                }
                WriteOp::Delete { key } => {
                    entries.remove(&key);
                }
                WriteOp::DeletePrefix { prefix } => {
                    let doomed: Vec<Bytes> = entries
                        .range(Self::range_bounds(&prefix))
                        .map(|(key, _)| key.clone())
                        .collect();
                    for key in doomed {
                        entries.remove(&key);
                    }
                }
            }
        }
        drop(entries);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mem_kv_conformance() {
        conformance::run(MemKv::new);
    }

    #[test]
    fn test_prefixed_conformance() {
        conformance::run(|| Prefixed::new(MemKv::new(), &b"ns/"[..]));
    }

    #[test]
    fn test_erased_conformance() {
        conformance::run(|| ErasedKv::into_dyn(MemKv::new()));
    }

    #[test]
    fn test_prefixed_isolates_namespaces() {
        let store = Arc::new(MemKv::new());
        let a = Prefixed::new(Arc::clone(&store), &b"a/"[..]);
        let b = Prefixed::new(Arc::clone(&store), &b"b/"[..]);
        let mut batch = WriteBatch::new();
        batch.put(&b"k"[..], &b"1"[..]);
        a.write_batch(batch).unwrap();
        assert_eq!(a.get(b"k").unwrap(), Some(Bytes::from_static(b"1")));
        assert_eq!(b.get(b"k").unwrap(), None);
        assert_eq!(b.get_prev(b"", b"z").unwrap(), None);
        let mut batch = WriteBatch::new();
        batch.delete_prefix(&b""[..]);
        b.write_batch(batch).unwrap();
        assert_eq!(a.get(b"k").unwrap(), Some(Bytes::from_static(b"1")));
    }

    #[test]
    fn test_namespaced_batches_apply_together() {
        let store = Arc::new(MemKv::new());
        let a = Prefixed::new(Arc::clone(&store), &b"a/"[..]);
        let b = Prefixed::new(Arc::clone(&store), &b"b/"[..]);
        let mut writes_a = WriteBatch::new();
        writes_a.put(&b"k"[..], &b"1"[..]);
        let mut writes_b = WriteBatch::new();
        writes_b.put(&b"k"[..], &b"2"[..]);
        let mut batch = a.namespace_batch(writes_a);
        batch.append(b.namespace_batch(writes_b));
        a.inner().write_batch(batch).unwrap();
        assert_eq!(a.get(b"k").unwrap(), Some(Bytes::from_static(b"1")));
        assert_eq!(b.get(b"k").unwrap(), Some(Bytes::from_static(b"2")));
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn test_prefix_end() {
        assert_eq!(prefix_end(b"ab"), Some(b"ac".to_vec()));
        assert_eq!(prefix_end(&[1, 0xff]), Some(vec![2]));
        assert_eq!(prefix_end(&[0xff, 0xff]), None);
        assert_eq!(prefix_end(b""), None);
    }

    #[test]
    fn test_mem_kv_counts_operations() {
        let store = MemKv::new();
        let mut batch = WriteBatch::new();
        batch.put(&b"a"[..], &b"1"[..]).delete(&b"b"[..]);
        store.write_batch(batch).unwrap();
        store.get(b"a").unwrap();
        store.get_prev(b"", b"b").unwrap();
        assert_eq!(
            store.stats(),
            MemKvStats {
                reads: 2,
                writes: 2
            }
        );
    }
}
