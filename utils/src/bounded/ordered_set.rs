use core::{
    hash::{BuildHasher, Hash},
    ops::Deref,
};
use std::collections::hash_map::RandomState;

use indexmap::{IndexSet, set};
use serde::{Deserialize, Deserializer};

use crate::{
    bounded::{
        Bounded, BoundedError, BoundedLen,
        collection::{self, BoundedCollection, SeqVisitor},
    },
    ordered_set::OrderedSet,
};

impl<T, S> BoundedLen for OrderedSet<T, S> {
    fn bounded_len(&self) -> usize {
        self.len()
    }
}

impl<T, S> BoundedCollection for OrderedSet<T, S>
where
    T: Eq + Hash,
    S: BuildHasher + Default,
{
    type Item = T;

    fn with_capacity(capacity: usize) -> Self {
        Self::from(IndexSet::with_capacity_and_hasher(capacity, S::default()))
    }

    fn add(&mut self, item: T) -> bool {
        self.insert(item)
    }
}

/// An [`OrderedSet`] whose element count is statically enforced to be in the
/// range `[MIN, MAX]`: a bounded vector whose elements are pairwise distinct.
///
/// A thin alias over [`Bounded`]. Every checked construction path
/// ([`TryFrom<Vec<T>>`](TryFrom), [`Self::try_from_iter`], deserialization)
/// enforces the bound and rejects a repeated element instead of dropping it,
/// so the elements read are always the elements held, in the order they were
/// read. Elements are appended with [`Self::try_push`] and removed by position
/// with [`Self::try_remove`] and [`Self::try_pop`], as on a vector.
///
/// Read access goes through `Deref` to the inner [`IndexSet`]. There is no
/// `DerefMut` and no mutable iteration: replacing an element in place could
/// repeat another one.
pub type BoundedOrderedSet<T, const MIN: usize, const MAX: usize, S = RandomState> =
    Bounded<OrderedSet<T, S>, MIN, MAX>;
/// A bounded ordered set containing between zero and `MAX` elements.
pub type UpperBoundedOrderedSet<T, const MAX: usize, S = RandomState> =
    BoundedOrderedSet<T, 0, MAX, S>;
/// A non-empty bounded ordered set containing at most `MAX` elements.
pub type NonEmptyBoundedOrderedSet<T, const MAX: usize, S = RandomState> =
    BoundedOrderedSet<T, 1, MAX, S>;

impl<T, S, const MIN: usize, const MAX: usize> BoundedOrderedSet<T, MIN, MAX, S>
where
    S: Default,
{
    /// Constructs an empty set.
    ///
    /// Only valid when `MIN` is zero: any other `MIN` fails to compile.
    #[must_use]
    pub fn empty() -> Self {
        const {
            assert!(
                MIN == 0,
                "Cannot construct empty BoundedOrderedSet when MIN > 0"
            );
        }
        Self::new_unchecked(OrderedSet::default())
    }
}

impl<T, S, const MIN: usize, const MAX: usize> BoundedOrderedSet<T, MIN, MAX, S>
where
    T: Eq + Hash,
    S: BuildHasher + Default,
{
    /// Constructs a bounded ordered set from an iterable of distinct elements,
    /// keeping them in iteration order.
    ///
    /// A repeated element is an error ([`BoundedError::DuplicateItem`]) rather
    /// than silently dropped. Iteration stops at the first duplicate or at the
    /// first element past `MAX`.
    pub fn try_from_iter<I>(iterable: I) -> Result<Self, BoundedError>
    where
        I: IntoIterator<Item = T>,
    {
        collection::collect_iter(iterable)
    }
}

impl<T, S, const MIN: usize, const MAX: usize> BoundedOrderedSet<T, MIN, MAX, S>
where
    T: Eq + Hash,
    S: BuildHasher,
{
    /// Appends `value` if it is new and doing so does not exceed `MAX`.
    ///
    /// Returns [`BoundedError::TooManyItems`] when the set already holds
    /// `MAX` elements, and [`BoundedError::DuplicateItem`] when `value` is
    /// already present; the set is left untouched either way.
    pub fn try_push(&mut self, value: T) -> Result<(), BoundedError> {
        let len = self.len();
        if len >= MAX {
            return Err(BoundedError::TooManyItems {
                count: len.saturating_add(1),
                max: MAX,
            });
        }
        if self.0.insert(value) {
            Ok(())
        } else {
            Err(BoundedError::DuplicateItem { index: len })
        }
    }

    /// Removes and returns the last element if the minimum length is kept.
    ///
    /// Returns `Ok(None)` when the set is empty or already at its minimum
    /// length.
    pub fn try_pop(&mut self) -> Result<Option<T>, BoundedError> {
        if self.is_empty() || self.len() - 1 < MIN {
            return Ok(None);
        }
        Ok(self.0.pop())
    }

    /// Removes and returns the element at `index` if the minimum length is
    /// kept, shifting every later element down by one so that order is
    /// preserved.
    ///
    /// This is an `O(n)` operation. Returns [`BoundedError::IndexOutOfBounds`]
    /// for an invalid index and [`BoundedError::TooFewItems`] when removing
    /// the element would violate `MIN`.
    pub fn try_remove(&mut self, index: usize) -> Result<T, BoundedError> {
        let len = self.len();
        if index >= len {
            return Err(BoundedError::IndexOutOfBounds { index, len });
        }
        let new_len = len - 1;
        if new_len < MIN {
            return Err(BoundedError::TooFewItems {
                count: new_len,
                min: MIN,
            });
        }
        Ok(self
            .0
            .shift_remove_index(index)
            .expect("index was checked against the length"))
    }
}

impl<T, S, const MIN: usize, const MAX: usize> Default for BoundedOrderedSet<T, MIN, MAX, S>
where
    S: Default,
{
    fn default() -> Self {
        Self::empty()
    }
}

impl<T, S, const MIN: usize, const MAX: usize> TryFrom<Vec<T>> for BoundedOrderedSet<T, MIN, MAX, S>
where
    T: Eq + Hash,
    S: BuildHasher + Default,
{
    type Error = BoundedError;

    fn try_from(value: Vec<T>) -> Result<Self, Self::Error> {
        Self::try_from_iter(value)
    }
}

impl<T, S, const MIN: usize, const MAX: usize> TryFrom<IndexSet<T, S>>
    for BoundedOrderedSet<T, MIN, MAX, S>
{
    type Error = BoundedError;

    fn try_from(value: IndexSet<T, S>) -> Result<Self, Self::Error> {
        Self::try_new(value.into())
    }
}

impl<T, S, const MIN: usize, const MAX: usize> From<BoundedOrderedSet<T, MIN, MAX, S>>
    for IndexSet<T, S>
{
    fn from(value: BoundedOrderedSet<T, MIN, MAX, S>) -> Self {
        value.into_inner().into()
    }
}

impl<T, S, const MIN: usize, const MAX: usize> From<BoundedOrderedSet<T, MIN, MAX, S>> for Vec<T> {
    fn from(value: BoundedOrderedSet<T, MIN, MAX, S>) -> Self {
        value.into_iter().collect()
    }
}

impl<T, S, const MIN: usize, const MAX: usize> Deref for BoundedOrderedSet<T, MIN, MAX, S> {
    type Target = OrderedSet<T, S>;

    fn deref(&self) -> &Self::Target {
        self.as_inner()
    }
}

impl<'a, T, S, const MIN: usize, const MAX: usize> IntoIterator
    for &'a BoundedOrderedSet<T, MIN, MAX, S>
{
    type Item = &'a T;
    type IntoIter = set::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<T, S, const MIN: usize, const MAX: usize> IntoIterator for BoundedOrderedSet<T, MIN, MAX, S> {
    type Item = T;
    type IntoIter = set::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_inner().into_iter()
    }
}

impl<'de, T, S, const MIN: usize, const MAX: usize> Deserialize<'de>
    for BoundedOrderedSet<T, MIN, MAX, S>
where
    T: Deserialize<'de> + Eq + Hash,
    S: BuildHasher + Default,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(SeqVisitor::new())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        hash::{BuildHasher as _, RandomState},
    };

    use indexmap::IndexSet;

    use crate::bounded::{BoundedError, BoundedOrderedSet, UpperBoundedOrderedSet};

    /// Concrete instantiation used across the tests: between 2 and 4 elements.
    type TestSet = BoundedOrderedSet<u8, 2, 4>;

    /// The elements of `set`, in order.
    fn elements(set: &TestSet) -> Vec<u8> {
        set.iter().copied().collect()
    }

    #[test]
    fn try_from_iter_keeps_iteration_order() {
        let set = TestSet::try_from_iter([3, 1, 2]).unwrap();

        assert_eq!(elements(&set), [3, 1, 2]);
    }

    #[test]
    fn try_from_iter_rejects_a_repeated_element_and_reports_its_position() {
        assert_eq!(
            TestSet::try_from_iter([1, 2, 1]),
            Err(BoundedError::DuplicateItem { index: 2 })
        );
    }

    #[test]
    fn try_from_iter_rejects_elements_outside_the_bounds() {
        assert_eq!(
            TestSet::try_from_iter([1]),
            Err(BoundedError::TooFewItems { count: 1, min: 2 })
        );
        assert_eq!(
            TestSet::try_from_iter([1, 2, 3, 4, 5]),
            Err(BoundedError::TooManyItems { count: 5, max: 4 })
        );
    }

    #[test]
    fn try_from_vec_checks_uniqueness_and_bounds() {
        assert_eq!(elements(&TestSet::try_from(vec![2, 1]).unwrap()), [2, 1]);
        assert_eq!(
            TestSet::try_from(vec![2, 1, 2]),
            Err(BoundedError::DuplicateItem { index: 2 })
        );
        assert_eq!(TestSet::try_from(vec![]), Err(BoundedError::EmptyInput));
    }

    #[test]
    fn try_from_index_set_checks_only_the_length() {
        let inner: IndexSet<u8> = [2, 1].into_iter().collect();

        let set = TestSet::try_from(inner).unwrap();

        assert_eq!(elements(&set), [2, 1]);
        assert_eq!(
            TestSet::try_from(IndexSet::new()),
            Err(BoundedError::EmptyInput)
        );
    }

    #[test]
    fn empty_and_default_build_an_empty_set() {
        assert!(UpperBoundedOrderedSet::<u8, 4>::empty().is_empty());
        assert!(UpperBoundedOrderedSet::<u8, 4>::default().is_empty());
    }

    #[test]
    fn try_push_appends_a_new_element_at_the_end() {
        let mut set = TestSet::try_from_iter([3, 1]).unwrap();

        assert_eq!(set.try_push(2), Ok(()));
        assert_eq!(elements(&set), [3, 1, 2]);
    }

    #[test]
    fn try_push_rejects_a_repeated_element_and_does_not_mutate() {
        let mut set = TestSet::try_from_iter([3, 1]).unwrap();

        assert_eq!(
            set.try_push(3),
            Err(BoundedError::DuplicateItem { index: 2 })
        );
        assert_eq!(elements(&set), [3, 1]);
    }

    #[test]
    fn try_push_rejects_growth_past_max() {
        let mut set = TestSet::try_from_iter([4, 3, 2, 1]).unwrap();

        assert_eq!(
            set.try_push(5),
            Err(BoundedError::TooManyItems { count: 5, max: 4 })
        );
        assert_eq!(elements(&set), [4, 3, 2, 1]);
    }

    #[test]
    fn try_pop_returns_none_at_or_below_lower_bound() {
        let mut set = TestSet::try_from_iter([1, 2, 3]).unwrap();

        assert_eq!(set.try_pop(), Ok(Some(3)));
        assert_eq!(set.try_pop(), Ok(None));
        assert_eq!(elements(&set), [1, 2]);
    }

    #[test]
    fn try_remove_removes_the_element_at_index_and_shifts_the_rest() {
        let mut set = TestSet::try_from_iter([4, 3, 2, 1]).unwrap();

        assert_eq!(set.try_remove(1), Ok(3));
        assert_eq!(elements(&set), [4, 2, 1]);
        assert!(!set.contains(&3));
    }

    #[test]
    fn try_remove_rejects_removal_below_min_and_out_of_bounds() {
        let mut set = TestSet::try_from_iter([1, 2]).unwrap();

        assert_eq!(
            set.try_remove(0),
            Err(BoundedError::TooFewItems { count: 1, min: 2 })
        );
        assert_eq!(
            set.try_remove(2),
            Err(BoundedError::IndexOutOfBounds { index: 2, len: 2 })
        );
        assert_eq!(elements(&set), [1, 2]);
    }

    #[test]
    fn index_access_follows_insertion_order() {
        let set = TestSet::try_from_iter([9, 5, 7]).unwrap();

        assert_eq!(set.get_index(1), Some(&5));
        assert_eq!(set.get_index_of(&7), Some(2));
        assert_eq!(set.first(), Some(&9));
        assert_eq!(set.last(), Some(&7));
    }

    #[test]
    fn into_iterator_and_into_vec_follow_insertion_order() {
        let set = TestSet::try_from_iter([2, 1]).unwrap();

        assert_eq!((&set).into_iter().copied().collect::<Vec<_>>(), [2, 1]);
        assert_eq!(Vec::from(set), [2, 1]);
    }

    #[test]
    fn equality_hashing_and_ordering_follow_the_sequence() {
        let forward = TestSet::try_from_iter([1, 2]).unwrap();
        let same = TestSet::try_from_iter([1, 2]).unwrap();
        let backward = TestSet::try_from_iter([2, 1]).unwrap();

        assert_eq!(forward, same);
        assert_ne!(forward, backward);
        assert!(forward < backward);
        // The wrapped `IndexSet` compares as a set and still calls them equal.
        assert_eq!(**forward.as_inner(), **backward.as_inner());

        let state = RandomState::new();
        assert_eq!(state.hash_one(&forward), state.hash_one(&same));
        let distinct: HashSet<TestSet> = [forward, same, backward].into_iter().collect();
        assert_eq!(distinct.len(), 2);
    }

    #[test]
    fn json_roundtrip_preserves_order() {
        let original = TestSet::try_from_iter([3, 1, 2]).unwrap();

        let json = serde_json::to_string(&original).unwrap();
        let restored: TestSet = serde_json::from_str(&json).unwrap();

        assert_eq!(json, "[3,1,2]");
        assert_eq!(restored, original);
    }

    #[test]
    fn binary_roundtrip_preserves_order_and_the_vector_wire_format() {
        let original = TestSet::try_from_iter([3, 1, 2]).unwrap();

        let encoded = bincode::serialize(&original).unwrap();
        let restored = bincode::deserialize::<TestSet>(&encoded).unwrap();

        assert_eq!(restored, original);
        assert_eq!(encoded, bincode::serialize(&vec![3u8, 1, 2]).unwrap());
    }

    #[test]
    fn deserialize_rejects_a_repeated_element() {
        let json = serde_json::from_str::<TestSet>("[1,2,1]").unwrap_err();
        assert!(
            json.to_string()
                .contains("Item at index 2 is a duplicate of an earlier item"),
            "unexpected error: {json}"
        );

        let encoded = bincode::serialize(&vec![1u8, 2, 1]).unwrap();
        let binary = bincode::deserialize::<TestSet>(&encoded).unwrap_err();
        assert!(
            binary.to_string().contains("duplicate"),
            "unexpected error: {binary}"
        );
    }

    #[test]
    fn deserialize_rejects_elements_outside_the_bounds() {
        let too_few = serde_json::from_str::<TestSet>("[1]").unwrap_err();
        assert!(
            too_few
                .to_string()
                .contains("Item count 1 is below minimum of 2"),
            "unexpected error: {too_few}"
        );

        let encoded = bincode::serialize(&vec![1u8, 2, 3, 4, 5]).unwrap();
        let too_many = bincode::deserialize::<TestSet>(&encoded).unwrap_err();
        assert!(
            too_many.to_string().contains("exceeds static maximum"),
            "unexpected error: {too_many}"
        );
    }

    /// Reserving the declared length outright would ask for `u64::MAX`
    /// elements and panic on capacity overflow, from an 8-byte input.
    #[test]
    fn deserialize_binary_does_not_preallocate_a_huge_declared_length() {
        type Unbounded = BoundedOrderedSet<u8, 0, { usize::MAX }>;

        let result = bincode::deserialize::<Unbounded>(&u64::MAX.to_le_bytes());

        assert!(result.is_err());
    }
}
