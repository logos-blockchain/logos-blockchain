use core::{borrow::Borrow, ops::Deref};
use std::collections::{BTreeSet, btree_set};

use serde::{Deserialize, Deserializer};

use crate::bounded::{
    Bounded, BoundedError, BoundedLen,
    collection::{self, BoundedCollection, SeqVisitor},
};

impl<T> BoundedLen for BTreeSet<T> {
    fn bounded_len(&self) -> usize {
        self.len()
    }
}

impl<T> BoundedCollection for BTreeSet<T>
where
    T: Ord,
{
    type Item = T;

    // A B-tree allocates node by node as it grows, so there is nothing to
    // reserve up front.
    fn with_capacity(_capacity: usize) -> Self {
        Self::new()
    }

    fn add(&mut self, item: T) -> bool {
        self.insert(item)
    }
}

/// A [`BTreeSet`] whose element count is statically enforced to be in the
/// range `[MIN, MAX]`.
///
/// A thin alias over [`Bounded`]. The elements are always held in increasing
/// order, whatever order they were given in, so two sets with the same
/// elements are equal, hash the same, and serialize the same.
///
/// Every checked construction path ([`TryFrom<Vec<T>>`](TryFrom),
/// [`Self::try_from_iter`]) enforces the bound and rejects a repeated element
/// instead of dropping it. Deserialization reads at most `MAX` elements,
/// repeats included; the elements may come in any order, and a repeated
/// element merges into the one already held, as it does for a [`BTreeSet`].
/// `MIN` applies to the elements held once repeats have merged. Elements are
/// added with [`Self::try_insert`] and removed with [`Self::try_remove`].
///
/// Read access goes through `Deref` to the inner [`BTreeSet`]. There is no
/// `DerefMut`: a mutable [`BTreeSet`] could change the length past the bound.
pub type BoundedBTreeSet<T, const MIN: usize, const MAX: usize> = Bounded<BTreeSet<T>, MIN, MAX>;
/// A bounded B-tree set containing between zero and `MAX` elements.
pub type UpperBoundedBTreeSet<T, const MAX: usize> = BoundedBTreeSet<T, 0, MAX>;
/// A non-empty bounded B-tree set containing at most `MAX` elements.
pub type NonEmptyBoundedBTreeSet<T, const MAX: usize> = BoundedBTreeSet<T, 1, MAX>;

impl<T, const MIN: usize, const MAX: usize> BoundedBTreeSet<T, MIN, MAX> {
    /// Constructs an empty set.
    ///
    /// Only valid when `MIN` is zero: any other `MIN` fails to compile.
    #[must_use]
    pub const fn empty() -> Self {
        const {
            assert!(
                MIN == 0,
                "Cannot construct empty BoundedBTreeSet when MIN > 0"
            );
        }
        Self::new_unchecked(BTreeSet::new())
    }
}

impl<T, const MIN: usize, const MAX: usize> BoundedBTreeSet<T, MIN, MAX>
where
    T: Ord,
{
    /// Constructs a bounded B-tree set from an iterable of distinct elements,
    /// in any order.
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

    /// Inserts `value` if it is new and doing so does not exceed `MAX`.
    ///
    /// Returns [`BoundedError::TooManyItems`] when the set already holds `MAX`
    /// elements, and [`BoundedError::DuplicateItem`] when `value` is already
    /// present; the set is left untouched either way.
    pub fn try_insert(&mut self, value: T) -> Result<(), BoundedError> {
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

    /// Removes `value`, if the minimum length is kept.
    ///
    /// Returns whether `value` was present, and [`BoundedError::TooFewItems`]
    /// when removing it would violate `MIN`; the set is left untouched then.
    pub fn try_remove<Q>(&mut self, value: &Q) -> Result<bool, BoundedError>
    where
        T: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if !self.contains(value) {
            return Ok(false);
        }
        let new_len = self.len() - 1;
        if new_len < MIN {
            return Err(BoundedError::TooFewItems {
                count: new_len,
                min: MIN,
            });
        }
        Ok(self.0.remove(value))
    }
}

impl<T, const MIN: usize, const MAX: usize> Default for BoundedBTreeSet<T, MIN, MAX> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<T, const MIN: usize, const MAX: usize> TryFrom<Vec<T>> for BoundedBTreeSet<T, MIN, MAX>
where
    T: Ord,
{
    type Error = BoundedError;

    fn try_from(value: Vec<T>) -> Result<Self, Self::Error> {
        Self::try_from_iter(value)
    }
}

impl<T, const MIN: usize, const MAX: usize> TryFrom<BTreeSet<T>> for BoundedBTreeSet<T, MIN, MAX> {
    type Error = BoundedError;

    fn try_from(value: BTreeSet<T>) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl<T, const MIN: usize, const MAX: usize> From<BoundedBTreeSet<T, MIN, MAX>> for BTreeSet<T> {
    fn from(value: BoundedBTreeSet<T, MIN, MAX>) -> Self {
        value.into_inner()
    }
}

impl<T, const MIN: usize, const MAX: usize> From<BoundedBTreeSet<T, MIN, MAX>> for Vec<T> {
    fn from(value: BoundedBTreeSet<T, MIN, MAX>) -> Self {
        value.into_iter().collect()
    }
}

impl<T, const MIN: usize, const MAX: usize> Deref for BoundedBTreeSet<T, MIN, MAX> {
    type Target = BTreeSet<T>;

    fn deref(&self) -> &Self::Target {
        self.as_inner()
    }
}

impl<T, const MIN: usize, const MAX: usize> From<T> for BoundedBTreeSet<T, MIN, MAX>
where
    T: Ord,
{
    fn from(value: T) -> Self {
        const {
            assert!(
                MIN <= 1,
                "Single-element construction is invalid for minimum bound > 1"
            );
            assert!(
                MAX >= 1,
                "Single-element construction is invalid for maximum bound < 1"
            );
        }
        Self::new_unchecked(BTreeSet::from([value]))
    }
}

impl<T, const MIN: usize, const MAX: usize, const INPUT_SIZE: usize> TryFrom<[T; INPUT_SIZE]>
    for BoundedBTreeSet<T, MIN, MAX>
where
    T: Ord,
{
    type Error = BoundedError;

    fn try_from(value: [T; INPUT_SIZE]) -> Result<Self, Self::Error> {
        const {
            assert!(
                MIN <= INPUT_SIZE,
                "Array construction is invalid for minimum bound > INPUT_SIZE"
            );
            assert!(
                MAX >= INPUT_SIZE,
                "Array construction is invalid for maximum bound < INPUT_SIZE"
            );
        }
        Self::try_from_iter(value)
    }
}

impl<'a, T, const MIN: usize, const MAX: usize> IntoIterator for &'a BoundedBTreeSet<T, MIN, MAX> {
    type Item = &'a T;
    type IntoIter = btree_set::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<T, const MIN: usize, const MAX: usize> IntoIterator for BoundedBTreeSet<T, MIN, MAX> {
    type Item = T;
    type IntoIter = btree_set::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_inner().into_iter()
    }
}

impl<'de, T, const MIN: usize, const MAX: usize> Deserialize<'de> for BoundedBTreeSet<T, MIN, MAX>
where
    T: Deserialize<'de> + Ord,
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
        collections::{BTreeSet, HashSet},
        hash::{BuildHasher as _, RandomState},
    };

    use crate::bounded::{
        BoundedBTreeSet, BoundedError, UpperBoundedBTreeSet,
        collection::test_utils::assert_serde_matches_underlying,
    };

    /// Concrete instantiation used across the tests: between 2 and 4 elements.
    type TestSet = BoundedBTreeSet<u8, 2, 4>;

    /// The elements of `set`, in order.
    fn elements(set: &TestSet) -> Vec<u8> {
        set.iter().copied().collect()
    }

    #[test]
    fn try_from_iter_sorts_the_elements() {
        let set = TestSet::try_from_iter([3, 1, 2]).unwrap();

        assert_eq!(elements(&set), [1, 2, 3]);
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
    fn try_from_vec_and_array_check_uniqueness_and_bounds() {
        assert_eq!(elements(&TestSet::try_from(vec![2, 1]).unwrap()), [1, 2]);
        assert_eq!(
            TestSet::try_from(vec![2, 1, 2]),
            Err(BoundedError::DuplicateItem { index: 2 })
        );
        assert_eq!(TestSet::try_from(vec![]), Err(BoundedError::EmptyInput));
        assert_eq!(
            TestSet::try_from([2, 2]),
            Err(BoundedError::DuplicateItem { index: 1 })
        );
    }

    #[test]
    fn try_from_btree_set_checks_only_the_length() {
        let set = TestSet::try_from(BTreeSet::from([2, 1])).unwrap();

        assert_eq!(elements(&set), [1, 2]);
        assert_eq!(
            TestSet::try_from(BTreeSet::new()),
            Err(BoundedError::EmptyInput)
        );
    }

    #[test]
    fn empty_default_and_single_element_construction() {
        assert!(UpperBoundedBTreeSet::<u8, 4>::empty().is_empty());
        assert!(UpperBoundedBTreeSet::<u8, 4>::default().is_empty());

        let single = UpperBoundedBTreeSet::<u8, 4>::from(7);
        assert_eq!(single.iter().collect::<Vec<_>>(), [&7]);
    }

    #[test]
    fn try_insert_keeps_the_elements_in_order() {
        let mut set = TestSet::try_from_iter([3, 1]).unwrap();

        assert_eq!(set.try_insert(2), Ok(()));
        assert_eq!(elements(&set), [1, 2, 3]);
    }

    #[test]
    fn try_insert_rejects_a_repeated_element_and_does_not_mutate() {
        let mut set = TestSet::try_from_iter([3, 1]).unwrap();

        assert_eq!(
            set.try_insert(3),
            Err(BoundedError::DuplicateItem { index: 2 })
        );
        assert_eq!(elements(&set), [1, 3]);
    }

    #[test]
    fn try_insert_rejects_growth_past_max() {
        let mut set = TestSet::try_from_iter([4, 3, 2, 1]).unwrap();

        assert_eq!(
            set.try_insert(5),
            Err(BoundedError::TooManyItems { count: 5, max: 4 })
        );
        assert_eq!(elements(&set), [1, 2, 3, 4]);
    }

    #[test]
    fn try_remove_removes_an_element() {
        let mut set = TestSet::try_from_iter([1, 2, 3]).unwrap();

        assert_eq!(set.try_remove(&2), Ok(true));
        assert_eq!(set.try_remove(&9), Ok(false));
        assert_eq!(elements(&set), [1, 3]);
    }

    #[test]
    fn try_remove_rejects_removal_below_min() {
        let mut set = TestSet::try_from_iter([1, 2]).unwrap();

        assert_eq!(
            set.try_remove(&1),
            Err(BoundedError::TooFewItems { count: 1, min: 2 })
        );
        // An absent element removes nothing, so it cannot break the bound.
        assert_eq!(set.try_remove(&9), Ok(false));
        assert_eq!(elements(&set), [1, 2]);
    }

    #[test]
    fn into_iterator_and_into_vec_follow_the_order() {
        let set = TestSet::try_from_iter([2, 1]).unwrap();

        assert_eq!((&set).into_iter().copied().collect::<Vec<_>>(), [1, 2]);
        assert_eq!(Vec::from(set), [1, 2]);
    }

    #[test]
    fn equality_and_hashing_ignore_the_construction_order() {
        let forward = TestSet::try_from_iter([1, 2]).unwrap();
        let backward = TestSet::try_from_iter([2, 1]).unwrap();
        let other = TestSet::try_from_iter([1, 3]).unwrap();

        assert_eq!(forward, backward);
        assert_ne!(forward, other);

        let state = RandomState::new();
        assert_eq!(state.hash_one(&forward), state.hash_one(&backward));
        let distinct: HashSet<TestSet> = [forward, backward, other].into_iter().collect();
        assert_eq!(distinct.len(), 2);
    }

    #[test]
    fn serialize_writes_the_elements_in_order() {
        let set = TestSet::try_from_iter([3, 1, 2]).unwrap();

        assert_eq!(serde_json::to_string(&set).unwrap(), "[1,2,3]");
        assert_eq!(
            bincode::serialize(&set).unwrap(),
            bincode::serialize(&vec![1u8, 2, 3]).unwrap()
        );
    }

    /// Within its bounds, a bounded B-tree set reads and writes exactly as a
    /// B-tree set does: the elements may come in any order, and a repeated
    /// element merges into the one already held.
    #[test]
    fn serde_matches_the_underlying_btree_set() {
        for elements in [&[2u8, 1][..], &[3, 1, 3, 2], &[1, 1, 2, 2]] {
            assert_serde_matches_underlying::<TestSet, BTreeSet<u8>>(
                &serde_json::to_string(elements).unwrap(),
                &bincode::serialize(elements).unwrap(),
            );
        }
    }

    /// Every element read counts against `MAX`, repeats included, so the work
    /// an input costs stays bounded. The element past `MAX` is refused where it
    /// arrives: the malformed element after it is never read.
    #[test]
    fn deserialize_refuses_the_element_past_maximum_even_when_repeated() {
        let json = serde_json::from_str::<TestSet>(r#"[1,1,1,1,1,"malformed"]"#).unwrap_err();
        assert!(
            json.to_string()
                .contains("Item count 5 exceeds static maximum of 4"),
            "unexpected error: {json}"
        );

        // A declared length past `MAX` is refused before any element is decoded.
        let encoded = bincode::serialize(&vec![1u8; 5]).unwrap();
        let binary = bincode::deserialize::<TestSet>(&encoded).unwrap_err();
        assert!(
            binary
                .to_string()
                .contains("Item count 5 exceeds static maximum of 4"),
            "unexpected error: {binary}"
        );
    }

    /// The minimum applies to the elements held, once repeats have merged.
    #[test]
    fn deserialize_checks_the_minimum_after_merging() {
        let err = serde_json::from_str::<TestSet>("[1,1]").unwrap_err();

        assert!(
            err.to_string()
                .contains("Item count 1 is below minimum of 2"),
            "unexpected error: {err}"
        );
    }

    /// Fewer elements than `MIN` cannot merge into more, so a declared length
    /// below it is refused before any element is decoded.
    #[test]
    fn deserialize_binary_rejects_a_declared_length_below_minimum() {
        let encoded = bincode::serialize(&vec![1u8]).unwrap();

        let err = bincode::deserialize::<TestSet>(&encoded).unwrap_err();

        assert!(
            err.to_string()
                .contains("Item count 1 is below minimum of 2"),
            "unexpected error: {err}"
        );
    }

    /// A declared length of `u64::MAX` elements, from an 8-byte input, fails
    /// on the missing elements rather than on an allocation.
    #[test]
    fn deserialize_binary_does_not_preallocate_a_huge_declared_length() {
        type Unbounded = BoundedBTreeSet<u8, 0, { usize::MAX }>;

        let result = bincode::deserialize::<Unbounded>(&u64::MAX.to_le_bytes());

        assert!(result.is_err());
    }
}
