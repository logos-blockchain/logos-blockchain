use core::{
    hash::{BuildHasher, Hash},
    ops::Deref,
};
use std::collections::hash_map::RandomState;

use indexmap::{
    Equivalent, IndexMap,
    map::{self, Entry},
};
use serde::{Deserialize, Deserializer};

use crate::bounded::{
    Bounded, BoundedError, BoundedLen,
    collection::{self, BoundedCollection, MapVisitor},
};

impl<K, V, S> BoundedLen for IndexMap<K, V, S> {
    fn bounded_len(&self) -> usize {
        self.len()
    }
}

impl<K, V, S> BoundedCollection for IndexMap<K, V, S>
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

/// An [`IndexMap`] whose entry count is statically enforced to be in the
/// range `[MIN, MAX]`, with unique keys kept in insertion order.
///
/// A thin alias over [`Bounded`]. Every checked construction path
/// ([`TryFrom<IndexMap>`](TryFrom), [`Self::try_from_iter`], deserialization)
/// enforces the bound, and the last two also reject a repeated key instead of
/// letting the later entry overwrite the earlier one.
///
/// Iteration, serialization and deserialization all follow insertion order.
///
/// # Equality ignores order
///
/// `==` compares as a map, the way [`IndexMap`] does: the same entries in a
/// different order are equal, even though they encode to different bytes.
/// Compare [`IndexMap::as_slice`] (reachable through `Deref`) when order must
/// count.
///
/// Read access goes through `Deref` to the inner [`IndexMap`]. There is no
/// `DerefMut`: values can be mutated in place, but every mutation that can
/// change the length goes through a checked method.
pub type BoundedIndexMap<K, V, const MIN: usize, const MAX: usize, S = RandomState> =
    Bounded<IndexMap<K, V, S>, MIN, MAX>;
/// A bounded index map containing between zero and `MAX` entries.
pub type UpperBoundedIndexMap<K, V, const MAX: usize, S = RandomState> =
    BoundedIndexMap<K, V, 0, MAX, S>;
/// A non-empty bounded index map containing at most `MAX` entries.
pub type NonEmptyBoundedIndexMap<K, V, const MAX: usize, S = RandomState> =
    BoundedIndexMap<K, V, 1, MAX, S>;

impl<K, V, S, const MIN: usize, const MAX: usize> BoundedIndexMap<K, V, MIN, MAX, S> {
    /// Returns the entry at position `index`, with a mutable reference to its
    /// value.
    pub fn get_index_mut(&mut self, index: usize) -> Option<(&K, &mut V)> {
        self.0.get_index_mut(index)
    }

    /// Returns an iterator over the entries in order, with mutable references
    /// to the values.
    pub fn iter_mut(&mut self) -> map::IterMut<'_, K, V> {
        self.0.iter_mut()
    }

    /// Returns an iterator over mutable references to the values, in order.
    pub fn values_mut(&mut self) -> map::ValuesMut<'_, K, V> {
        self.0.values_mut()
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> BoundedIndexMap<K, V, MIN, MAX, S>
where
    S: Default,
{
    /// Constructs an empty map.
    ///
    /// Only valid when `MIN` is zero: any other `MIN` fails to compile.
    #[must_use]
    pub fn empty() -> Self {
        const {
            assert!(
                MIN == 0,
                "Cannot construct empty BoundedIndexMap when MIN > 0"
            );
        }
        Self::new_unchecked(IndexMap::default())
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> BoundedIndexMap<K, V, MIN, MAX, S>
where
    K: Eq + Hash,
    S: BuildHasher + Default,
{
    /// Constructs a bounded map from an iterable of entries with distinct
    /// keys, keeping them in iteration order.
    ///
    /// Unlike collecting into an [`IndexMap`], a repeated key is an error
    /// ([`BoundedError::DuplicateItem`]) rather than an overwrite. Iteration
    /// stops at the first duplicate or at the first entry past `MAX`.
    pub fn try_from_iter<I>(iterable: I) -> Result<Self, BoundedError>
    where
        I: IntoIterator<Item = (K, V)>,
    {
        collection::collect_iter(iterable)
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> BoundedIndexMap<K, V, MIN, MAX, S>
where
    K: Eq + Hash,
    S: BuildHasher,
{
    /// Inserts `value` under `key` if doing so does not exceed `MAX`.
    ///
    /// A new key is appended after every existing entry. Replacing the value of
    /// a key that is already present keeps that key's position, never changes
    /// the length, and so succeeds even when the map is full, returning the
    /// previous value. Returns [`BoundedError::TooManyItems`] when `key` is new
    /// and the map already holds `MAX` entries.
    pub fn try_insert(&mut self, key: K, value: V) -> Result<Option<V>, BoundedError> {
        let len = self.len();
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
}

impl<K, V, S, const MIN: usize, const MAX: usize> BoundedIndexMap<K, V, MIN, MAX, S>
where
    S: BuildHasher,
{
    /// Removes the entry under `key` if doing so keeps at least `MIN` entries,
    /// shifting every later entry down by one so that order is preserved.
    ///
    /// This is an `O(n)` operation. Returns the removed value, or `Ok(None)`
    /// if `key` was absent. Returns [`BoundedError::TooFewItems`], leaving the
    /// map untouched, when removing the entry would violate `MIN`.
    pub fn try_shift_remove<Q>(&mut self, key: &Q) -> Result<Option<V>, BoundedError>
    where
        Q: Hash + Equivalent<K> + ?Sized,
    {
        if !self.contains_key(key) {
            return Ok(None);
        }
        // If there was one element to remove, length is not zero, so decrementing is
        // safe here.
        let remaining = self.len() - 1;
        if remaining < MIN {
            return Err(BoundedError::TooFewItems {
                count: remaining,
                min: MIN,
            });
        }
        Ok(self.0.shift_remove(key))
    }

    /// Returns a mutable reference to the value under `key`.
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        Q: Hash + Equivalent<K> + ?Sized,
    {
        self.0.get_mut(key)
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> Default for BoundedIndexMap<K, V, MIN, MAX, S>
where
    S: Default,
{
    fn default() -> Self {
        Self::empty()
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> TryFrom<IndexMap<K, V, S>>
    for BoundedIndexMap<K, V, MIN, MAX, S>
{
    type Error = BoundedError;

    fn try_from(value: IndexMap<K, V, S>) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> From<BoundedIndexMap<K, V, MIN, MAX, S>>
    for IndexMap<K, V, S>
{
    fn from(value: BoundedIndexMap<K, V, MIN, MAX, S>) -> Self {
        value.into_inner()
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> Deref for BoundedIndexMap<K, V, MIN, MAX, S> {
    type Target = IndexMap<K, V, S>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'a, K, V, S, const MIN: usize, const MAX: usize> IntoIterator
    for &'a BoundedIndexMap<K, V, MIN, MAX, S>
{
    type Item = (&'a K, &'a V);
    type IntoIter = map::Iter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, K, V, S, const MIN: usize, const MAX: usize> IntoIterator
    for &'a mut BoundedIndexMap<K, V, MIN, MAX, S>
{
    type Item = (&'a K, &'a mut V);
    type IntoIter = map::IterMut<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> IntoIterator
    for BoundedIndexMap<K, V, MIN, MAX, S>
{
    type Item = (K, V);
    type IntoIter = map::IntoIter<K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_inner().into_iter()
    }
}

impl<'de, K, V, S, const MIN: usize, const MAX: usize> Deserialize<'de>
    for BoundedIndexMap<K, V, MIN, MAX, S>
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
    use indexmap::IndexMap;

    use crate::bounded::{BoundedError, BoundedIndexMap, UpperBoundedIndexMap};

    /// Concrete instantiation used across the tests: between 2 and 4 entries.
    type TestMap = BoundedIndexMap<u8, u16, 2, 4>;

    /// The entries of `map`, in order.
    fn entries(map: &TestMap) -> Vec<(u8, u16)> {
        map.iter().map(|(key, value)| (*key, *value)).collect()
    }

    #[test]
    fn try_from_iter_keeps_iteration_order() {
        let map = TestMap::try_from_iter([(3, 30), (1, 10), (2, 20)]).unwrap();

        assert_eq!(entries(&map), [(3, 30), (1, 10), (2, 20)]);
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
    fn try_from_index_map_checks_only_the_length() {
        let inner: IndexMap<u8, u16> = [(2, 20), (1, 10)].into_iter().collect();

        let map = TestMap::try_from(inner).unwrap();

        assert_eq!(entries(&map), [(2, 20), (1, 10)]);
        assert_eq!(
            TestMap::try_from(IndexMap::new()),
            Err(BoundedError::EmptyInput)
        );
    }

    #[test]
    fn empty_and_default_build_an_empty_map() {
        assert!(UpperBoundedIndexMap::<u8, u16, 4>::empty().is_empty());
        assert!(UpperBoundedIndexMap::<u8, u16, 4>::default().is_empty());
    }

    #[test]
    fn try_insert_appends_a_new_key_at_the_end() {
        let mut map = TestMap::try_from_iter([(3, 30), (1, 10)]).unwrap();

        assert_eq!(map.try_insert(2, 20), Ok(None));
        assert_eq!(entries(&map), [(3, 30), (1, 10), (2, 20)]);
    }

    #[test]
    fn try_insert_replaces_an_existing_value_in_place_even_when_full() {
        let mut map = TestMap::try_from_iter([(4, 40), (3, 30), (2, 20), (1, 10)]).unwrap();

        assert_eq!(map.try_insert(3, 33), Ok(Some(30)));
        assert_eq!(entries(&map), [(4, 40), (3, 33), (2, 20), (1, 10)]);
    }

    #[test]
    fn try_insert_rejects_a_new_key_when_full() {
        let mut map = TestMap::try_from_iter([(4, 40), (3, 30), (2, 20), (1, 10)]).unwrap();

        assert_eq!(
            map.try_insert(5, 50),
            Err(BoundedError::TooManyItems { count: 5, max: 4 })
        );
        assert_eq!(entries(&map), [(4, 40), (3, 30), (2, 20), (1, 10)]);
    }

    #[test]
    fn try_shift_remove_preserves_the_order_of_the_remaining_entries() {
        let mut map = TestMap::try_from_iter([(4, 40), (3, 30), (2, 20), (1, 10)]).unwrap();

        assert_eq!(map.try_shift_remove(&3), Ok(Some(30)));
        assert_eq!(entries(&map), [(4, 40), (2, 20), (1, 10)]);
    }

    #[test]
    fn try_shift_remove_reports_an_absent_key_even_at_the_minimum() {
        let mut map = TestMap::try_from_iter([(1, 10), (2, 20)]).unwrap();

        assert_eq!(map.try_shift_remove(&9), Ok(None));
    }

    #[test]
    fn try_shift_remove_rejects_removal_below_the_minimum() {
        let mut map = TestMap::try_from_iter([(1, 10), (2, 20)]).unwrap();

        assert_eq!(
            map.try_shift_remove(&1),
            Err(BoundedError::TooFewItems { count: 1, min: 2 })
        );
        assert_eq!(entries(&map), [(1, 10), (2, 20)]);
    }

    #[test]
    fn values_can_be_mutated_in_place_without_reordering() {
        let mut map = TestMap::try_from_iter([(2, 20), (1, 10)]).unwrap();

        *map.get_mut(&1).unwrap() += 1;
        *map.get_index_mut(0).unwrap().1 += 2;
        for value in map.values_mut() {
            *value *= 10;
        }
        for (_, value) in &mut map {
            *value += 1;
        }

        assert_eq!(entries(&map), [(2, 221), (1, 111)]);
    }

    #[test]
    fn index_access_follows_insertion_order() {
        let map = TestMap::try_from_iter([(9, 90), (5, 50), (7, 70)]).unwrap();

        assert_eq!(map.get_index(1), Some((&5, &50)));
        assert_eq!(map.get_index_of(&7), Some(2));
        assert_eq!(map.first(), Some((&9, &90)));
        assert_eq!(map.last(), Some((&7, &70)));
    }

    #[test]
    fn into_iterator_by_value_follows_insertion_order() {
        let map = TestMap::try_from_iter([(2, 20), (1, 10)]).unwrap();

        assert_eq!(map.into_iter().collect::<Vec<_>>(), [(2, 20), (1, 10)]);
    }

    #[test]
    fn equality_ignores_order_but_slice_comparison_does_not() {
        let forward = TestMap::try_from_iter([(1, 10), (2, 20)]).unwrap();
        let backward = TestMap::try_from_iter([(2, 20), (1, 10)]).unwrap();

        assert_eq!(forward, backward);
        assert_ne!(forward.as_slice(), backward.as_slice());
        assert_ne!(
            bincode::serialize(&forward).unwrap(),
            bincode::serialize(&backward).unwrap()
        );
    }

    #[test]
    fn json_roundtrip_preserves_order() {
        let original = TestMap::try_from_iter([(3, 30), (1, 10), (2, 20)]).unwrap();

        let json = serde_json::to_string(&original).unwrap();
        let restored: TestMap = serde_json::from_str(&json).unwrap();

        assert_eq!(json, r#"{"3":30,"1":10,"2":20}"#);
        assert_eq!(restored.as_slice(), original.as_slice());
    }

    #[test]
    fn binary_roundtrip_preserves_order_and_the_map_wire_format() {
        let original = TestMap::try_from_iter([(3, 30), (1, 10), (2, 20)]).unwrap();

        let encoded = bincode::serialize(&original).unwrap();
        let restored = bincode::deserialize::<TestMap>(&encoded).unwrap();

        assert_eq!(restored.as_slice(), original.as_slice());
        assert_eq!(encoded, bincode::serialize(original.as_inner()).unwrap());
        assert_eq!(
            encoded,
            bincode::serialize(&vec![(3u8, 30u16), (1, 10), (2, 20)]).unwrap()
        );
    }

    #[test]
    fn deserialize_rejects_a_repeated_key() {
        let json = serde_json::from_str::<TestMap>(r#"{"1":10,"2":20,"1":99}"#).unwrap_err();
        assert!(
            json.to_string()
                .contains("Item at index 2 is a duplicate of an earlier item"),
            "unexpected error: {json}"
        );

        let encoded = bincode::serialize(&vec![(1u8, 10u16), (2, 20), (1, 99)]).unwrap();
        let binary = bincode::deserialize::<TestMap>(&encoded).unwrap_err();
        assert!(
            binary.to_string().contains("duplicate"),
            "unexpected error: {binary}"
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

        let encoded =
            bincode::serialize(&vec![(1u8, 1u16), (2, 2), (3, 3), (4, 4), (5, 5)]).unwrap();
        let too_many = bincode::deserialize::<TestMap>(&encoded).unwrap_err();
        assert!(
            too_many.to_string().contains("exceeds static maximum"),
            "unexpected error: {too_many}"
        );
    }

    /// Reserving the declared length outright would ask for `u64::MAX`
    /// entries and panic on capacity overflow, from an 8-byte input.
    #[test]
    fn deserialize_binary_does_not_preallocate_a_huge_declared_length() {
        type Unbounded = BoundedIndexMap<u8, u8, 0, { usize::MAX }>;

        let result = bincode::deserialize::<Unbounded>(&u64::MAX.to_le_bytes());

        assert!(result.is_err());
    }
}
