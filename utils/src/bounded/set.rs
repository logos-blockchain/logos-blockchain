use core::{
    borrow::Borrow,
    hash::{BuildHasher, Hash},
    ops::Deref,
};
use std::collections::{HashSet, hash_map::RandomState, hash_set};

use serde::{Deserialize, Deserializer};

use crate::bounded::{
    Bounded, BoundedError, BoundedLen,
    collection::{self, BoundedCollection, SeqVisitor},
};

impl<T, S> BoundedLen for HashSet<T, S> {
    fn bounded_len(&self) -> usize {
        self.len()
    }
}

impl<T, S> BoundedCollection for HashSet<T, S>
where
    T: Eq + Hash,
    S: BuildHasher + Default,
{
    type Item = T;

    fn with_capacity(capacity: usize) -> Self {
        Self::with_capacity_and_hasher(capacity, S::default())
    }

    fn add(&mut self, item: T) -> bool {
        self.insert(item)
    }
}

/// A [`HashSet`] whose element count is statically enforced to be in the
/// range `[MIN, MAX]`.
///
/// A thin alias over [`Bounded`]. Every checked construction path
/// ([`TryFrom<HashSet>`](TryFrom), [`Self::try_from_iter`], deserialization)
/// enforces the bound, and the last two also reject a repeated element instead
/// of dropping it.
///
/// Read access goes through `Deref` to the inner [`HashSet`]. There is no
/// `DerefMut`: every mutation that can change the length goes through a
/// checked method.
pub type BoundedSet<T, const MIN: usize, const MAX: usize, S = RandomState> =
    Bounded<HashSet<T, S>, MIN, MAX>;
/// A bounded set containing between zero and `MAX` elements.
pub type UpperBoundedSet<T, const MAX: usize, S = RandomState> = BoundedSet<T, 0, MAX, S>;
/// A non-empty bounded set containing at most `MAX` elements.
pub type NonEmptyBoundedSet<T, const MAX: usize, S = RandomState> = BoundedSet<T, 1, MAX, S>;

impl<T, S, const MIN: usize, const MAX: usize> BoundedSet<T, MIN, MAX, S>
where
    S: Default,
{
    /// Constructs an empty set.
    ///
    /// Only valid when `MIN` is zero: any other `MIN` fails to compile.
    #[must_use]
    pub fn empty() -> Self {
        const { assert!(MIN == 0, "Cannot construct empty BoundedSet when MIN > 0") }
        Self::new_unchecked(HashSet::default())
    }
}

impl<T, S, const MIN: usize, const MAX: usize> BoundedSet<T, MIN, MAX, S>
where
    T: Eq + Hash,
    S: BuildHasher + Default,
{
    /// Constructs a bounded set from an iterable of distinct elements.
    ///
    /// Unlike collecting into a [`HashSet`], a repeated element is an error
    /// ([`BoundedError::DuplicateItem`]) rather than silently dropped.
    /// Iteration stops at the first duplicate or at the first element past
    /// `MAX`.
    pub fn try_from_iter<I>(iterable: I) -> Result<Self, BoundedError>
    where
        I: IntoIterator<Item = T>,
    {
        collection::collect_iter(iterable)
    }
}

impl<T, S, const MIN: usize, const MAX: usize> BoundedSet<T, MIN, MAX, S>
where
    T: Eq + Hash,
    S: BuildHasher,
{
    /// Adds `value` if doing so does not exceed `MAX`.
    ///
    /// Returns `Ok(true)` if `value` was inserted and `Ok(false)` if it was
    /// already present, which never changes the length and so succeeds even
    /// when the set is full. Returns [`BoundedError::TooManyItems`] when
    /// `value` is new and the set already holds `MAX` elements.
    pub fn try_insert(&mut self, value: T) -> Result<bool, BoundedError> {
        if self.0.len() < MAX {
            return Ok(self.0.insert(value));
        }
        if self.0.contains(&value) {
            return Ok(false);
        }
        Err(BoundedError::TooManyItems {
            count: self.0.len().saturating_add(1),
            max: MAX,
        })
    }

    /// Removes `value` if doing so keeps at least `MIN` elements.
    ///
    /// Returns `Ok(true)` if `value` was removed and `Ok(false)` if it was
    /// absent. Returns [`BoundedError::TooFewItems`], leaving the set
    /// untouched, when removing `value` would violate `MIN`.
    pub fn try_remove<Value>(&mut self, value: &Value) -> Result<bool, BoundedError>
    where
        T: Borrow<Value>,
        Value: Eq + Hash + ?Sized,
    {
        if !self.0.contains(value) {
            return Ok(false);
        }
        // If there was one element to remove, length is not zero, so decrementing is
        // safe here.
        let remaining = self.0.len() - 1;
        if remaining < MIN {
            return Err(BoundedError::TooFewItems {
                count: remaining,
                min: MIN,
            });
        }
        Ok(self.0.remove(value))
    }
}

impl<T, S, const MIN: usize, const MAX: usize> Default for Bounded<HashSet<T, S>, MIN, MAX>
where
    S: Default,
{
    fn default() -> Self {
        Self::empty()
    }
}

impl<T, S, const MIN: usize, const MAX: usize> TryFrom<HashSet<T, S>>
    for Bounded<HashSet<T, S>, MIN, MAX>
{
    type Error = BoundedError;

    fn try_from(value: HashSet<T, S>) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl<T, S, const MIN: usize, const MAX: usize> From<Bounded<Self, MIN, MAX>> for HashSet<T, S> {
    fn from(value: Bounded<Self, MIN, MAX>) -> Self {
        value.into_inner()
    }
}

impl<T, S, const MIN: usize, const MAX: usize> Deref for Bounded<HashSet<T, S>, MIN, MAX> {
    type Target = HashSet<T, S>;

    fn deref(&self) -> &Self::Target {
        self.as_inner()
    }
}

impl<'a, T, S, const MIN: usize, const MAX: usize> IntoIterator
    for &'a Bounded<HashSet<T, S>, MIN, MAX>
{
    type Item = &'a T;
    type IntoIter = hash_set::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_inner().iter()
    }
}

impl<T, S, const MIN: usize, const MAX: usize> IntoIterator for Bounded<HashSet<T, S>, MIN, MAX> {
    type Item = T;
    type IntoIter = hash_set::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_inner().into_iter()
    }
}

impl<'de, T, S, const MIN: usize, const MAX: usize> Deserialize<'de>
    for Bounded<HashSet<T, S>, MIN, MAX>
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
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use serde::{Deserialize, Deserializer};

    use crate::bounded::{BoundedError, BoundedSet, UpperBoundedSet};

    /// Concrete instantiation used across the tests: between 2 and 4 elements.
    type TestSet = BoundedSet<u8, 2, 4>;

    static ELEMENT_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
    static ELEMENT_ATTEMPTS_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Counts how many elements the deserializer attempts to decode.
    #[derive(PartialEq, Eq, Hash)]
    struct CountingByte(u8);

    impl<'de> Deserialize<'de> for CountingByte {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            ELEMENT_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
            u8::deserialize(deserializer).map(Self)
        }
    }

    fn set_of(items: &[u8]) -> HashSet<u8> {
        items.iter().copied().collect()
    }

    #[test]
    fn try_from_iter_accepts_distinct_items_within_bounds() {
        let set = TestSet::try_from_iter([3, 1, 2]).unwrap();

        assert_eq!(*set, set_of(&[1, 2, 3]));
    }

    #[test]
    fn try_from_iter_rejects_a_duplicate_and_reports_its_position() {
        assert_eq!(
            TestSet::try_from_iter([1, 2, 1]),
            Err(BoundedError::DuplicateItem { index: 2 })
        );
    }

    #[test]
    fn try_from_iter_does_not_let_duplicates_satisfy_the_minimum() {
        // Deduplicating `[7, 7]` would leave a single element, below `MIN`.
        assert_eq!(
            TestSet::try_from_iter([7, 7]),
            Err(BoundedError::DuplicateItem { index: 1 })
        );
    }

    #[test]
    fn try_from_iter_rejects_items_below_minimum() {
        assert_eq!(
            TestSet::try_from_iter([1]),
            Err(BoundedError::TooFewItems { count: 1, min: 2 })
        );
        assert_eq!(TestSet::try_from_iter([]), Err(BoundedError::EmptyInput));
    }

    #[test]
    fn try_from_iter_rejects_items_above_maximum() {
        assert_eq!(
            TestSet::try_from_iter([1, 2, 3, 4, 5]),
            Err(BoundedError::TooManyItems { count: 5, max: 4 })
        );
    }

    #[test]
    fn try_from_iter_stops_one_item_past_maximum() {
        let mut pulled = 0;
        let result = TestSet::try_from_iter((0..).inspect(|_| pulled += 1));

        assert_eq!(result, Err(BoundedError::TooManyItems { count: 5, max: 4 }));
        assert_eq!(pulled, 5);
    }

    #[test]
    fn try_from_hash_set_checks_only_the_length() {
        assert_eq!(TestSet::try_from(set_of(&[1, 2, 3])).unwrap().len(), 3);
        assert_eq!(
            TestSet::try_from(set_of(&[1])),
            Err(BoundedError::TooFewItems { count: 1, min: 2 })
        );
        assert_eq!(
            TestSet::try_from(set_of(&[1, 2, 3, 4, 5])),
            Err(BoundedError::TooManyItems { count: 5, max: 4 })
        );
    }

    #[test]
    fn empty_and_default_build_an_empty_set() {
        assert!(UpperBoundedSet::<u8, 4>::empty().is_empty());
        assert!(UpperBoundedSet::<u8, 4>::default().is_empty());

        /*
           This does not compile, because `MIN` is not zero:
           ```
           let empty = TestSet::empty();
           ```
        */
    }

    #[test]
    fn try_insert_adds_new_elements_while_under_the_cap() {
        let mut set = TestSet::try_from_iter([1, 2]).unwrap();

        assert_eq!(set.try_insert(3), Ok(true));
        assert_eq!(set.try_insert(4), Ok(true));
        assert_eq!(*set, set_of(&[1, 2, 3, 4]));
    }

    #[test]
    fn try_insert_accepts_an_existing_element_even_when_full() {
        let mut set = TestSet::try_from_iter([1, 2, 3, 4]).unwrap();

        assert_eq!(set.try_insert(3), Ok(false));
        assert_eq!(*set, set_of(&[1, 2, 3, 4]));
    }

    #[test]
    fn try_insert_rejects_a_new_element_when_full() {
        let mut set = TestSet::try_from_iter([1, 2, 3, 4]).unwrap();

        assert_eq!(
            set.try_insert(5),
            Err(BoundedError::TooManyItems { count: 5, max: 4 })
        );
        assert_eq!(*set, set_of(&[1, 2, 3, 4]));
    }

    #[test]
    fn try_remove_removes_an_element_above_the_minimum() {
        let mut set = TestSet::try_from_iter([1, 2, 3]).unwrap();

        assert_eq!(set.try_remove(&2), Ok(true));
        assert_eq!(*set, set_of(&[1, 3]));
    }

    #[test]
    fn try_remove_reports_an_absent_element_even_at_the_minimum() {
        let mut set = TestSet::try_from_iter([1, 2]).unwrap();

        assert_eq!(set.try_remove(&9), Ok(false));
        assert_eq!(*set, set_of(&[1, 2]));
    }

    #[test]
    fn try_remove_rejects_removal_below_the_minimum() {
        let mut set = TestSet::try_from_iter([1, 2]).unwrap();

        assert_eq!(
            set.try_remove(&1),
            Err(BoundedError::TooFewItems { count: 1, min: 2 })
        );
        assert_eq!(*set, set_of(&[1, 2]));
    }

    #[test]
    fn into_iterator_by_reference_and_by_value() {
        let set = TestSet::try_from_iter([1, 2, 3]).unwrap();

        let borrowed: HashSet<u8> = (&set).into_iter().copied().collect();
        assert_eq!(borrowed, set_of(&[1, 2, 3]));

        let owned: HashSet<u8> = set.into_iter().collect();
        assert_eq!(owned, set_of(&[1, 2, 3]));
    }

    #[test]
    fn into_hash_set_unwraps_the_bounded_set() {
        let set = TestSet::try_from_iter([1, 2]).unwrap();

        assert_eq!(HashSet::from(set), set_of(&[1, 2]));
    }

    #[test]
    fn serialize_then_deserialize_roundtrips_through_json() {
        let original = TestSet::try_from_iter([5, 6, 7]).unwrap();

        let json = serde_json::to_string(&original).unwrap();
        let restored: TestSet = serde_json::from_str(&json).unwrap();

        assert_eq!(restored, original);
    }

    #[test]
    fn deserialize_rejects_a_duplicate_element() {
        let err = serde_json::from_str::<TestSet>("[1,2,1]").unwrap_err();

        assert!(
            err.to_string()
                .contains("Item at index 2 is a duplicate of an earlier item"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn deserialize_rejects_input_below_min() {
        let err = serde_json::from_str::<TestSet>("[1]").unwrap_err();

        assert!(
            err.to_string()
                .contains("Item count 1 is below minimum of 2"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn deserialize_rejects_input_above_max() {
        let err = serde_json::from_str::<TestSet>("[1,2,3,4,5]").unwrap_err();

        assert!(
            err.to_string().contains("exceeds static maximum"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn deserialize_json_stops_after_at_most_one_element_past_maximum() {
        let _test_guard = ELEMENT_ATTEMPTS_TEST_LOCK.lock().unwrap();
        ELEMENT_ATTEMPTS.store(0, Ordering::Relaxed);

        let result = serde_json::from_str::<BoundedSet<CountingByte, 0, 4>>("[1,2,3,4,5,6]");

        assert!(result.is_err());
        assert!(ELEMENT_ATTEMPTS.load(Ordering::Relaxed) <= 5);
    }

    #[test]
    fn deserialize_binary_rejects_oversized_length_before_decoding_elements() {
        let _test_guard = ELEMENT_ATTEMPTS_TEST_LOCK.lock().unwrap();
        ELEMENT_ATTEMPTS.store(0, Ordering::Relaxed);
        let encoded = bincode::serialize(&vec![1u8, 2, 3, 4, 5]).unwrap();

        let result = bincode::deserialize::<BoundedSet<CountingByte, 0, 4>>(&encoded);

        assert!(result.is_err());
        assert_eq!(ELEMENT_ATTEMPTS.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn deserialize_binary_rejects_undersized_length_before_decoding_elements() {
        let _test_guard = ELEMENT_ATTEMPTS_TEST_LOCK.lock().unwrap();
        ELEMENT_ATTEMPTS.store(0, Ordering::Relaxed);
        let encoded = bincode::serialize(&vec![1u8]).unwrap();

        let result = bincode::deserialize::<BoundedSet<CountingByte, 2, 4>>(&encoded);

        assert!(result.is_err());
        assert_eq!(ELEMENT_ATTEMPTS.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn deserialize_binary_rejects_a_duplicate_element() {
        let encoded = bincode::serialize(&vec![1u8, 2, 1]).unwrap();

        let err = bincode::deserialize::<TestSet>(&encoded).unwrap_err();

        assert!(
            err.to_string().contains("duplicate"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn deserialize_binary_preserves_the_set_wire_format() {
        let original = TestSet::try_from_iter([5, 6, 7]).unwrap();

        let encoded = bincode::serialize(&original).unwrap();
        let restored = bincode::deserialize::<TestSet>(&encoded).unwrap();

        assert_eq!(restored, original);
        assert_eq!(encoded, bincode::serialize(original.as_inner()).unwrap());
    }

    /// Reserving the declared length outright would ask for `u64::MAX`
    /// elements and panic on capacity overflow, from an 8-byte input.
    #[test]
    fn deserialize_binary_does_not_preallocate_a_huge_declared_length() {
        type Unbounded = BoundedSet<u8, 0, { usize::MAX }>;

        let result = bincode::deserialize::<Unbounded>(&u64::MAX.to_le_bytes());

        assert!(result.is_err());
    }
}
