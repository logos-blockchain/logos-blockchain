use core::{
    cmp::Ordering,
    fmt::{self, Debug, Formatter},
    hash::{Hash, Hasher},
    ops::{Deref, DerefMut},
};
use std::hash::RandomState;

use indexmap::{IndexSet, set};
use serde::Serialize;

/// A set that keeps its elements in the order they were inserted and compares
/// as the sequence it holds.
///
/// Membership is a set's: each element appears at most once, and lookup takes
/// constant time. Everything else is a vector's. Iteration, indexing,
/// serialization and the canonical encoding all follow the insertion order,
/// and so do equality, hashing and ordering. Two ordered sets are equal exactly
/// when they hold the same elements in the same order, which is exactly when
/// they encode to the same bytes.
#[derive(Clone, Serialize)]
#[serde(bound(serialize = "T: Serialize"), transparent)]
pub struct OrderedSet<T, S = RandomState>(IndexSet<T, S>);

impl<T, S> Debug for OrderedSet<T, S>
where
    T: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        // Printed as the sequence it compares as, not as a set.
        self.as_slice().fmt(f)
    }
}

impl<T, S> PartialEq for OrderedSet<T, S>
where
    T: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T, S> Eq for OrderedSet<T, S> where T: Eq {}

impl<T, S> Hash for OrderedSet<T, S>
where
    T: Hash,
{
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        self.as_slice().hash(state);
    }
}

impl<T, S> PartialOrd for OrderedSet<T, S>
where
    T: PartialOrd,
{
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.as_slice().partial_cmp(other.as_slice())
    }
}

impl<T, S> Ord for OrderedSet<T, S>
where
    T: Ord,
{
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_slice().cmp(other.as_slice())
    }
}

impl<T, S> Default for OrderedSet<T, S>
where
    S: Default,
{
    fn default() -> Self {
        Self(IndexSet::default())
    }
}

impl<T, S> Deref for OrderedSet<T, S> {
    type Target = IndexSet<T, S>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T, S> DerefMut for OrderedSet<T, S> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T, S> From<IndexSet<T, S>> for OrderedSet<T, S> {
    fn from(set: IndexSet<T, S>) -> Self {
        Self(set)
    }
}

impl<T, S> From<OrderedSet<T, S>> for IndexSet<T, S> {
    fn from(set: OrderedSet<T, S>) -> Self {
        set.0
    }
}

impl<T, S> IntoIterator for OrderedSet<T, S> {
    type Item = T;
    type IntoIter = set::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}
