use core::{
    borrow::Borrow,
    hash::{BuildHasher, Hash},
    ops::Deref,
};
use std::collections::{
    HashMap,
    hash_map::{self, Entry, RandomState},
};

use serde::{Deserialize, Deserializer};

use crate::bounded::{
    Bounded, BoundedError, BoundedLen,
    collection::{self, BoundedCollection, MapVisitor},
};

impl<K, V, S> BoundedLen for HashMap<K, V, S> {
    fn bounded_len(&self) -> usize {
        self.len()
    }
}

impl<K, V, S> BoundedCollection for HashMap<K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher + Default,
{
    type Item = (K, V);

    fn with_capacity(capacity: usize) -> Self {
        Self::with_capacity_and_hasher(capacity, S::default())
    }

    fn add(&mut self, (key, value): (K, V)) -> bool {
        match self.entry(key) {
            Entry::Occupied(_) => false,
            Entry::Vacant(entry) => {
                entry.insert(value);
                true
            }
        }
    }
}

/// A [`HashMap`] whose entry count is statically enforced to be in the range
/// `[MIN, MAX]`.
///
/// A thin alias over [`Bounded`]. Every checked construction path
/// ([`TryFrom<HashMap>`](TryFrom), [`Self::try_from_iter`], deserialization)
/// enforces the bound, and the last two also reject a repeated key instead of
/// letting the later entry overwrite the earlier one.
///
/// Read access goes through `Deref` to the inner [`HashMap`]. There is no
/// `DerefMut`: values can be mutated in place, but every mutation that can
/// change the length goes through a checked method.
pub type BoundedMap<K, V, const MIN: usize, const MAX: usize, S = RandomState> =
    Bounded<HashMap<K, V, S>, MIN, MAX>;
/// A bounded map containing between zero and `MAX` entries.
pub type UpperBoundedMap<K, V, const MAX: usize, S = RandomState> = BoundedMap<K, V, 0, MAX, S>;
/// A non-empty bounded map containing at most `MAX` entries.
pub type NonEmptyBoundedMap<K, V, const MAX: usize, S = RandomState> = BoundedMap<K, V, 1, MAX, S>;

impl<K, V, S, const MIN: usize, const MAX: usize> Bounded<HashMap<K, V, S>, MIN, MAX> {
    /// Returns an iterator over the entries, with mutable references to the
    /// values, in unspecified order.
    pub fn iter_mut(&mut self) -> hash_map::IterMut<'_, K, V> {
        self.0.iter_mut()
    }

    /// Returns an iterator over mutable references to the values, in
    /// unspecified order.
    pub fn values_mut(&mut self) -> hash_map::ValuesMut<'_, K, V> {
        self.0.values_mut()
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> Bounded<HashMap<K, V, S>, MIN, MAX>
where
    S: Default,
{
    /// Constructs an empty map.
    ///
    /// Only valid when `MIN` is zero: any other `MIN` fails to compile.
    #[must_use]
    pub fn empty() -> Self
    where
        S: Default,
    {
        const { assert!(MIN == 0, "Cannot construct empty BoundedMap when MIN > 0") }
        Self::new_unchecked(HashMap::default())
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> Bounded<HashMap<K, V, S>, MIN, MAX>
where
    K: Eq + Hash,
    S: BuildHasher + Default,
{
    /// Constructs a bounded map from an iterable of entries with distinct
    /// keys.
    ///
    /// Unlike collecting into a [`HashMap`], a repeated key is an error
    /// ([`BoundedError::DuplicateItem`]) rather than an overwrite. Iteration
    /// stops at the first duplicate or at the first entry past `MAX`.
    pub fn try_from_iter<I>(iterable: I) -> Result<Self, BoundedError>
    where
        I: IntoIterator<Item = (K, V)>,
    {
        collection::collect_iter(iterable)
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> Bounded<HashMap<K, V, S>, MIN, MAX>
where
    K: Eq + Hash,
    S: BuildHasher,
{
    /// Inserts `value` under `key` if doing so does not exceed `MAX`.
    ///
    /// Replacing the value of a key that is already present never changes the
    /// length, so it succeeds even when the map is full and returns the
    /// previous value. Returns [`BoundedError::TooManyItems`] when `key` is new
    /// and the map already holds `MAX` entries.
    pub fn try_insert(&mut self, key: K, value: V) -> Result<Option<V>, BoundedError> {
        let len = self.0.len();
        match self.0.entry(key) {
            Entry::Occupied(mut entry) => Ok(Some(entry.insert(value))),
            Entry::Vacant(_) if len >= MAX => Err(BoundedError::TooManyItems {
                count: len.saturating_add(1),
                max: MAX,
            }),
            Entry::Vacant(entry) => {
                entry.insert(value);
                Ok(None)
            }
        }
    }

    /// Removes the entry under `key` if doing so keeps at least `MIN` entries.
    ///
    /// Returns the removed value, or `Ok(None)` if `key` was absent. Returns
    /// [`BoundedError::TooFewItems`], leaving the map untouched, when removing
    /// the entry would violate `MIN`.
    pub fn try_remove<Q>(&mut self, key: &Q) -> Result<Option<V>, BoundedError>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        if !self.0.contains_key(key) {
            return Ok(None);
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
        Ok(self.0.remove(key))
    }

    /// Returns a mutable reference to the value under `key`.
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q> + Eq + Hash,
        Q: Eq + Hash + ?Sized,
        S: BuildHasher,
    {
        self.0.get_mut(key)
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> Default for Bounded<HashMap<K, V, S>, MIN, MAX>
where
    S: Default,
{
    fn default() -> Self {
        Self::empty()
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> TryFrom<HashMap<K, V, S>>
    for Bounded<HashMap<K, V, S>, MIN, MAX>
{
    type Error = BoundedError;

    fn try_from(value: HashMap<K, V, S>) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> From<Bounded<Self, MIN, MAX>>
    for HashMap<K, V, S>
{
    fn from(value: Bounded<Self, MIN, MAX>) -> Self {
        value.into_inner()
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> Deref for Bounded<HashMap<K, V, S>, MIN, MAX> {
    type Target = HashMap<K, V, S>;

    fn deref(&self) -> &Self::Target {
        self.as_inner()
    }
}

impl<'a, K, V, S, const MIN: usize, const MAX: usize> IntoIterator
    for &'a Bounded<HashMap<K, V, S>, MIN, MAX>
{
    type Item = (&'a K, &'a V);
    type IntoIter = hash_map::Iter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_inner().iter()
    }
}

impl<'a, K, V, S, const MIN: usize, const MAX: usize> IntoIterator
    for &'a mut Bounded<HashMap<K, V, S>, MIN, MAX>
{
    type Item = (&'a K, &'a mut V);
    type IntoIter = hash_map::IterMut<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> IntoIterator
    for Bounded<HashMap<K, V, S>, MIN, MAX>
{
    type Item = (K, V);
    type IntoIter = hash_map::IntoIter<K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_inner().into_iter()
    }
}

impl<'de, K, V, S, const MIN: usize, const MAX: usize> Deserialize<'de>
    for Bounded<HashMap<K, V, S>, MIN, MAX>
where
    K: Deserialize<'de> + Eq + Hash,
    V: Deserialize<'de>,
    S: BuildHasher + Default,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(MapVisitor::new())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use serde::{Deserialize, Deserializer};

    use crate::bounded::{BoundedError, BoundedMap, UpperBoundedMap};

    /// Concrete instantiation used across the tests: between 2 and 4 entries.
    type TestMap = BoundedMap<u8, u16, 2, 4>;

    static ENTRY_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
    static ENTRY_ATTEMPTS_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Counts how many values the deserializer attempts to decode.
    struct CountingValue;

    impl<'de> Deserialize<'de> for CountingValue {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            ENTRY_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
            u16::deserialize(deserializer).map(|_| Self)
        }
    }

    fn map_of(entries: &[(u8, u16)]) -> HashMap<u8, u16> {
        entries.iter().copied().collect()
    }

    #[test]
    fn try_from_iter_accepts_distinct_keys_within_bounds() {
        let map = TestMap::try_from_iter([(1, 10), (2, 20), (3, 30)]).unwrap();

        assert_eq!(*map, map_of(&[(1, 10), (2, 20), (3, 30)]));
    }

    #[test]
    fn try_from_iter_rejects_a_repeated_key_even_with_a_different_value() {
        assert_eq!(
            TestMap::try_from_iter([(1, 10), (2, 20), (1, 99)]),
            Err(BoundedError::DuplicateItem { index: 2 })
        );
    }

    #[test]
    fn try_from_iter_rejects_entries_outside_the_bounds() {
        assert_eq!(
            TestMap::try_from_iter([(1, 10)]),
            Err(BoundedError::TooFewItems { count: 1, min: 2 })
        );
        assert_eq!(
            TestMap::try_from_iter([(1, 10), (2, 20), (3, 30), (4, 40), (5, 50)]),
            Err(BoundedError::TooManyItems { count: 5, max: 4 })
        );
    }

    #[test]
    fn try_from_hash_map_checks_only_the_length() {
        assert_eq!(
            TestMap::try_from(map_of(&[(1, 10), (2, 20)]))
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            TestMap::try_from(HashMap::new()),
            Err(BoundedError::EmptyInput)
        );
    }

    #[test]
    fn empty_and_default_build_an_empty_map() {
        assert!(UpperBoundedMap::<u8, u16, 4>::empty().is_empty());
        assert!(UpperBoundedMap::<u8, u16, 4>::default().is_empty());
    }

    #[test]
    fn try_insert_adds_new_keys_while_under_the_cap() {
        let mut map = TestMap::try_from_iter([(1, 10), (2, 20)]).unwrap();

        assert_eq!(map.try_insert(3, 30), Ok(None));
        assert_eq!(*map, map_of(&[(1, 10), (2, 20), (3, 30)]));
    }

    #[test]
    fn try_insert_replaces_the_value_of_an_existing_key_even_when_full() {
        let mut map = TestMap::try_from_iter([(1, 10), (2, 20), (3, 30), (4, 40)]).unwrap();

        assert_eq!(map.try_insert(2, 22), Ok(Some(20)));
        assert_eq!(*map, map_of(&[(1, 10), (2, 22), (3, 30), (4, 40)]));
    }

    #[test]
    fn try_insert_rejects_a_new_key_when_full() {
        let mut map = TestMap::try_from_iter([(1, 10), (2, 20), (3, 30), (4, 40)]).unwrap();

        assert_eq!(
            map.try_insert(5, 50),
            Err(BoundedError::TooManyItems { count: 5, max: 4 })
        );
        assert_eq!(*map, map_of(&[(1, 10), (2, 20), (3, 30), (4, 40)]));
    }

    #[test]
    fn try_remove_returns_the_removed_value_above_the_minimum() {
        let mut map = TestMap::try_from_iter([(1, 10), (2, 20), (3, 30)]).unwrap();

        assert_eq!(map.try_remove(&2), Ok(Some(20)));
        assert_eq!(*map, map_of(&[(1, 10), (3, 30)]));
    }

    #[test]
    fn try_remove_reports_an_absent_key_even_at_the_minimum() {
        let mut map = TestMap::try_from_iter([(1, 10), (2, 20)]).unwrap();

        assert_eq!(map.try_remove(&9), Ok(None));
    }

    #[test]
    fn try_remove_rejects_removal_below_the_minimum() {
        let mut map = TestMap::try_from_iter([(1, 10), (2, 20)]).unwrap();

        assert_eq!(
            map.try_remove(&1),
            Err(BoundedError::TooFewItems { count: 1, min: 2 })
        );
        assert_eq!(*map, map_of(&[(1, 10), (2, 20)]));
    }

    #[test]
    fn values_can_be_mutated_in_place() {
        let mut map = TestMap::try_from_iter([(1, 10), (2, 20)]).unwrap();

        *map.get_mut(&1).unwrap() += 1;
        for value in map.values_mut() {
            *value *= 2;
        }
        for (_, value) in &mut map {
            *value += 1;
        }

        assert_eq!(*map, map_of(&[(1, 23), (2, 41)]));
    }

    #[test]
    fn into_hash_map_unwraps_the_bounded_map() {
        let map = TestMap::try_from_iter([(1, 10), (2, 20)]).unwrap();

        assert_eq!(HashMap::from(map), map_of(&[(1, 10), (2, 20)]));
    }

    #[test]
    fn serialize_then_deserialize_roundtrips_through_json() {
        let original = TestMap::try_from_iter([(1, 10), (2, 20), (3, 30)]).unwrap();

        let json = serde_json::to_string(&original).unwrap();
        let restored: TestMap = serde_json::from_str(&json).unwrap();

        assert_eq!(restored, original);
    }

    #[test]
    fn deserialize_rejects_a_repeated_key() {
        let err = serde_json::from_str::<TestMap>(r#"{"1":10,"2":20,"1":99}"#).unwrap_err();

        assert!(
            err.to_string()
                .contains("Item at index 2 is a duplicate of an earlier item"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn deserialize_rejects_entries_outside_the_bounds() {
        let too_few = serde_json::from_str::<TestMap>(r#"{"1":10}"#).unwrap_err();
        assert!(
            too_few
                .to_string()
                .contains("Item count 1 is below minimum of 2"),
            "unexpected error: {too_few}"
        );

        let too_many =
            serde_json::from_str::<TestMap>(r#"{"1":1,"2":2,"3":3,"4":4,"5":5}"#).unwrap_err();
        assert!(
            too_many.to_string().contains("exceeds static maximum"),
            "unexpected error: {too_many}"
        );
    }

    #[test]
    fn deserialize_json_stops_after_at_most_one_entry_past_maximum() {
        let _test_guard = ENTRY_ATTEMPTS_TEST_LOCK.lock().unwrap();
        ENTRY_ATTEMPTS.store(0, Ordering::Relaxed);

        let result = serde_json::from_str::<BoundedMap<u8, CountingValue, 0, 4>>(
            r#"{"1":1,"2":2,"3":3,"4":4,"5":5,"6":6}"#,
        );

        assert!(result.is_err());
        assert!(ENTRY_ATTEMPTS.load(Ordering::Relaxed) <= 5);
    }

    #[test]
    fn deserialize_binary_rejects_oversized_length_before_decoding_entries() {
        let _test_guard = ENTRY_ATTEMPTS_TEST_LOCK.lock().unwrap();
        ENTRY_ATTEMPTS.store(0, Ordering::Relaxed);
        let encoded =
            bincode::serialize(&map_of(&[(1, 1), (2, 2), (3, 3), (4, 4), (5, 5)])).unwrap();

        let result = bincode::deserialize::<BoundedMap<u8, CountingValue, 0, 4>>(&encoded);

        assert!(result.is_err());
        assert_eq!(ENTRY_ATTEMPTS.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn deserialize_binary_rejects_undersized_length_before_decoding_entries() {
        let _test_guard = ENTRY_ATTEMPTS_TEST_LOCK.lock().unwrap();
        ENTRY_ATTEMPTS.store(0, Ordering::Relaxed);
        let encoded = bincode::serialize(&map_of(&[(1, 1)])).unwrap();

        let result = bincode::deserialize::<BoundedMap<u8, CountingValue, 2, 4>>(&encoded);

        assert!(result.is_err());
        assert_eq!(ENTRY_ATTEMPTS.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn deserialize_binary_rejects_a_repeated_key() {
        // Configured bincode encodes a map exactly like a sequence of pairs,
        // which is the only way to put a repeated key on the wire.
        let encoded = bincode::serialize(&vec![(1u8, 10u16), (2, 20), (1, 99)]).unwrap();

        let err = bincode::deserialize::<TestMap>(&encoded).unwrap_err();

        assert!(
            err.to_string().contains("duplicate"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn deserialize_binary_preserves_the_map_wire_format() {
        let original = TestMap::try_from_iter([(1, 10), (2, 20), (3, 30)]).unwrap();

        let encoded = bincode::serialize(&original).unwrap();
        let restored = bincode::deserialize::<TestMap>(&encoded).unwrap();

        assert_eq!(restored, original);
        assert_eq!(encoded, bincode::serialize(original.as_inner()).unwrap());
    }

    /// Reserving the declared length outright would ask for `u64::MAX`
    /// entries and panic on capacity overflow, from an 8-byte input.
    #[test]
    fn deserialize_binary_does_not_preallocate_a_huge_declared_length() {
        type Unbounded = BoundedMap<u8, u8, 0, { usize::MAX }>;

        let result = bincode::deserialize::<Unbounded>(&u64::MAX.to_le_bytes());

        assert!(result.is_err());
    }
}
