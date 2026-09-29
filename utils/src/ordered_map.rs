use core::{
    cmp::Ordering,
    fmt::{self, Debug, Formatter},
    hash::{Hash, Hasher},
    ops::{Deref, DerefMut},
};
use std::hash::RandomState;

use indexmap::{IndexMap, map};
use serde::Serialize;

/// A map that keeps its entries in the order they were inserted and compares
/// as the sequence it holds.
///
/// Key lookup is a map's: each key appears at most once, and lookup takes
/// constant time. Everything else is a vector of pairs'. Iteration, indexing,
/// serialization and the canonical encoding all follow the insertion order,
/// and so do equality, hashing and ordering. Two ordered maps are equal exactly
/// when they hold the same entries in the same order, which is exactly when
/// they encode to the same bytes.
#[derive(Clone, Serialize)]
#[serde(bound(serialize = "K: Serialize, V: Serialize"), transparent)]
pub struct OrderedMap<K, V, S = RandomState>(IndexMap<K, V, S>);

impl<K, V, S> Debug for OrderedMap<K, V, S>
where
    K: Debug,
    V: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        // Printed as the sequence it compares as.
        self.as_slice().fmt(f)
    }
}

impl<K, V, S> PartialEq for OrderedMap<K, V, S>
where
    K: PartialEq,
    V: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<K, V, S> Eq for OrderedMap<K, V, S>
where
    K: Eq,
    V: Eq,
{
}

impl<K, V, S> Hash for OrderedMap<K, V, S>
where
    K: Hash,
    V: Hash,
{
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        self.as_slice().hash(state);
    }
}

impl<K, V, S> PartialOrd for OrderedMap<K, V, S>
where
    K: PartialOrd,
    V: PartialOrd,
{
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.as_slice().partial_cmp(other.as_slice())
    }
}

impl<K, V, S> Ord for OrderedMap<K, V, S>
where
    K: Ord,
    V: Ord,
{
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_slice().cmp(other.as_slice())
    }
}

impl<K, V, S> Default for OrderedMap<K, V, S>
where
    S: Default,
{
    fn default() -> Self {
        Self(IndexMap::default())
    }
}

impl<K, V, S> Deref for OrderedMap<K, V, S> {
    type Target = IndexMap<K, V, S>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<K, V, S> DerefMut for OrderedMap<K, V, S> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<K, V, S> From<IndexMap<K, V, S>> for OrderedMap<K, V, S> {
    fn from(map: IndexMap<K, V, S>) -> Self {
        Self(map)
    }
}

impl<K, V, S> From<OrderedMap<K, V, S>> for IndexMap<K, V, S> {
    fn from(map: OrderedMap<K, V, S>) -> Self {
        map.0
    }
}

impl<K, V, S> IntoIterator for OrderedMap<K, V, S> {
    type Item = (K, V);
    type IntoIter = map::IntoIter<K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}
