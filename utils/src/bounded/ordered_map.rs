use core::{
    fmt,
    hash::{BuildHasher, Hash},
    marker::PhantomData,
    ops::Deref,
};
use std::collections::hash_map::RandomState;

use indexmap::{
    Equivalent, IndexMap,
    map::{self, Entry},
};
use serde::{
    Deserialize, Deserializer,
    de::{Error as _, MapAccess, Visitor},
};

use crate::{
    bounded::{
        Bounded, BoundedError, BoundedLen,
        collection::{self, BoundedCollection, check_declared_len, collect},
    },
    ordered_map::OrderedMap,
};

impl<K, V, S> BoundedLen for OrderedMap<K, V, S> {
    fn bounded_len(&self) -> usize {
        self.len()
    }
}

impl<K, V, S> BoundedCollection for OrderedMap<K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher + Default,
{
    type Item = (K, V);

    fn with_capacity(capacity: usize) -> Self {
        Self::from(IndexMap::with_capacity_and_hasher(capacity, S::default()))
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

/// An [`OrderedMap`] whose entry count is statically enforced to be in the
/// range `[MIN, MAX]`: a bounded vector of pairs whose keys are pairwise
/// distinct.
///
/// A thin alias over [`Bounded`]. Every checked construction path
/// ([`TryFrom<Vec<(K, V)>>`](TryFrom), [`Self::try_from_iter`],
/// deserialization) enforces the bound and rejects a repeated key instead of
/// letting the later entry overwrite the earlier one, so the entries read are
/// always the entries held, in the order they were read. Entries are appended
/// with [`Self::try_push`] and removed by position with [`Self::try_remove`]
/// and [`Self::try_pop`], as on a vector. Values can be changed in place; keys
/// cannot, since a changed key could repeat another.
///
/// Read access goes through `Deref` to the inner [`IndexMap`]. There is no
/// `DerefMut`: a mutable [`IndexMap`] could change the length past the bound.
pub type BoundedOrderedMap<K, V, const MIN: usize, const MAX: usize, S = RandomState> =
    Bounded<OrderedMap<K, V, S>, MIN, MAX>;
/// A bounded ordered map containing between zero and `MAX` entries.
pub type UpperBoundedOrderedMap<K, V, const MAX: usize, S = RandomState> =
    BoundedOrderedMap<K, V, 0, MAX, S>;
/// A non-empty bounded ordered map containing at most `MAX` entries.
pub type NonEmptyBoundedOrderedMap<K, V, const MAX: usize, S = RandomState> =
    BoundedOrderedMap<K, V, 1, MAX, S>;

impl<K, V, S, const MIN: usize, const MAX: usize> BoundedOrderedMap<K, V, MIN, MAX, S> {
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

impl<K, V, S, const MIN: usize, const MAX: usize> BoundedOrderedMap<K, V, MIN, MAX, S>
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
                "Cannot construct empty BoundedOrderedMap when MIN > 0"
            );
        }
        Self::new_unchecked(OrderedMap::default())
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> BoundedOrderedMap<K, V, MIN, MAX, S>
where
    K: Eq + Hash,
    S: BuildHasher + Default,
{
    /// Constructs a bounded ordered map from an iterable of entries with
    /// distinct keys, keeping them in iteration order.
    ///
    /// A repeated key is an error ([`BoundedError::DuplicateItem`]) rather
    /// than an overwrite. Iteration stops at the first duplicate or at the
    /// first entry past `MAX`.
    pub fn try_from_iter<I>(iterable: I) -> Result<Self, BoundedError>
    where
        I: IntoIterator<Item = (K, V)>,
    {
        collection::collect_iter(iterable)
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> BoundedOrderedMap<K, V, MIN, MAX, S>
where
    K: Eq + Hash,
    S: BuildHasher,
{
    /// Appends the entry `key => value` if `key` is new and doing so does not
    /// exceed `MAX`.
    ///
    /// Returns [`BoundedError::TooManyItems`] when the map already holds `MAX`
    /// entries, and [`BoundedError::DuplicateItem`] when `key` is already
    /// present; the map is left untouched either way. Use [`Self::get_mut`] to
    /// change the value of a present key.
    pub fn try_insert(&mut self, key: K, value: V) -> Result<(), BoundedError> {
        let len = self.len();
        if len >= MAX {
            return Err(BoundedError::TooManyItems {
                count: len.saturating_add(1),
                max: MAX,
            });
        }
        match self.0.entry(key) {
            Entry::Occupied(_) => Err(BoundedError::DuplicateItem { index: len }),
            Entry::Vacant(entry) => {
                entry.insert(value);
                Ok(())
            }
        }
    }

    /// Removes and returns the last entry if the minimum length is kept.
    ///
    /// Returns `Ok(None)` when the map is empty or already at its minimum
    /// length.
    pub fn try_pop(&mut self) -> Result<Option<(K, V)>, BoundedError> {
        if self.is_empty() || self.len() - 1 < MIN {
            return Ok(None);
        }
        Ok(self.0.pop())
    }

    /// Removes and returns the entry at `index` if the minimum length is kept,
    /// shifting every later entry down by one so that order is preserved.
    ///
    /// This is an `O(n)` operation. Returns [`BoundedError::IndexOutOfBounds`]
    /// for an invalid index and [`BoundedError::TooFewItems`] when removing
    /// the entry would violate `MIN`.
    pub fn try_remove(&mut self, index: usize) -> Result<(K, V), BoundedError> {
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

    /// Returns a mutable reference to the value under `key`.
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        Q: Hash + Equivalent<K> + ?Sized,
    {
        self.0.get_mut(key)
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> Default for BoundedOrderedMap<K, V, MIN, MAX, S>
where
    S: Default,
{
    fn default() -> Self {
        Self::empty()
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> TryFrom<Vec<(K, V)>>
    for BoundedOrderedMap<K, V, MIN, MAX, S>
where
    K: Eq + Hash,
    S: BuildHasher + Default,
{
    type Error = BoundedError;

    fn try_from(value: Vec<(K, V)>) -> Result<Self, Self::Error> {
        Self::try_from_iter(value)
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> TryFrom<IndexMap<K, V, S>>
    for BoundedOrderedMap<K, V, MIN, MAX, S>
{
    type Error = BoundedError;

    fn try_from(value: IndexMap<K, V, S>) -> Result<Self, Self::Error> {
        Self::try_new(value.into())
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> From<BoundedOrderedMap<K, V, MIN, MAX, S>>
    for IndexMap<K, V, S>
{
    fn from(value: BoundedOrderedMap<K, V, MIN, MAX, S>) -> Self {
        value.into_inner().into()
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> From<BoundedOrderedMap<K, V, MIN, MAX, S>>
    for Vec<(K, V)>
{
    fn from(value: BoundedOrderedMap<K, V, MIN, MAX, S>) -> Self {
        value.into_iter().collect()
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> Deref for BoundedOrderedMap<K, V, MIN, MAX, S> {
    type Target = IndexMap<K, V, S>;

    fn deref(&self) -> &Self::Target {
        self.as_inner()
    }
}

impl<'a, K, V, S, const MIN: usize, const MAX: usize> IntoIterator
    for &'a BoundedOrderedMap<K, V, MIN, MAX, S>
{
    type Item = (&'a K, &'a V);
    type IntoIter = map::Iter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, K, V, S, const MIN: usize, const MAX: usize> IntoIterator
    for &'a mut BoundedOrderedMap<K, V, MIN, MAX, S>
{
    type Item = (&'a K, &'a mut V);
    type IntoIter = map::IterMut<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> IntoIterator
    for BoundedOrderedMap<K, V, MIN, MAX, S>
{
    type Item = (K, V);
    type IntoIter = map::IntoIter<K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_inner().into_iter()
    }
}

impl<'de, K, V, S, const MIN: usize, const MAX: usize> Deserialize<'de>
    for BoundedOrderedMap<K, V, MIN, MAX, S>
where
    K: Deserialize<'de> + Eq + Hash,
    V: Deserialize<'de>,
    S: BuildHasher + Default,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(MapVisitor(PhantomData))
    }
}

/// Deserializes a map into a [`BoundedOrderedMap`], reading each entry key
/// first.
///
/// An entry the map cannot admit, one past `MAX` or one repeating a key, is
/// refused on its key, so its value is never decoded and the refusal is the
/// error reported. The builder repeats both checks, as it does for every
/// bounded collection.
struct MapVisitor<K, V, S, const MIN: usize, const MAX: usize>(PhantomData<OrderedMap<K, V, S>>);

impl<'de, K, V, S, const MIN: usize, const MAX: usize> Visitor<'de>
    for MapVisitor<K, V, S, MIN, MAX>
where
    K: Deserialize<'de> + Eq + Hash,
    V: Deserialize<'de>,
    S: BuildHasher + Default,
{
    type Value = BoundedOrderedMap<K, V, MIN, MAX, S>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "a map of between {MIN} and {MAX} entries with distinct keys"
        )
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let hint = map.size_hint();
        check_declared_len::<OrderedMap<K, V, S>, A::Error, MIN, MAX>(hint)?;
        collect(
            hint,
            |built: &OrderedMap<K, V, S>| {
                let Some(key) = map.next_key()? else {
                    return Ok(None);
                };
                // Refuse on the key, before the value is decoded.
                let index = built.len();
                if index >= MAX {
                    return Err(A::Error::custom(BoundedError::TooManyItems {
                        count: index.saturating_add(1),
                        max: MAX,
                    }));
                }
                if built.contains_key(&key) {
                    return Err(A::Error::custom(BoundedError::DuplicateItem { index }));
                }
                Ok(Some((key, map.next_value()?)))
            },
            A::Error::custom,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        hash::{BuildHasher as _, RandomState},
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use indexmap::IndexMap;
    use serde::{Deserialize, Deserializer};

    use crate::bounded::{BoundedError, BoundedOrderedMap, UpperBoundedOrderedMap};

    /// Concrete instantiation used across the tests: between 2 and 4 entries.
    type TestMap = BoundedOrderedMap<u8, u16, 2, 4>;

    /// Like [`TestMap`], but its values count how often they are decoded.
    type CountingMap = BoundedOrderedMap<u8, CountingValue, 0, 4>;

    static VALUE_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
    static VALUE_ATTEMPTS_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// A `u16` value that records every attempt to deserialize it.
    #[derive(Debug)]
    struct CountingValue;

    impl<'de> Deserialize<'de> for CountingValue {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            VALUE_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
            u16::deserialize(deserializer).map(|_| Self)
        }
    }

    /// Deserializes `input` with `deserialize`, and returns its error with the
    /// number of values it attempted to decode.
    fn error_and_value_attempts<Input>(
        input: Input,
        deserialize: impl FnOnce(Input) -> Result<CountingMap, String>,
    ) -> (String, usize) {
        let _test_guard = VALUE_ATTEMPTS_TEST_LOCK.lock().unwrap();
        VALUE_ATTEMPTS.store(0, Ordering::Relaxed);

        let error = deserialize(input).unwrap_err();

        (error, VALUE_ATTEMPTS.load(Ordering::Relaxed))
    }

    fn from_json(json: &str) -> Result<CountingMap, String> {
        serde_json::from_str(json).map_err(|error| error.to_string())
    }

    fn from_bincode(bytes: &[u8]) -> Result<CountingMap, String> {
        bincode::deserialize(bytes).map_err(|error| error.to_string())
    }

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
    fn try_from_vec_checks_uniqueness_and_bounds() {
        assert_eq!(
            entries(&TestMap::try_from(vec![(2, 20), (1, 10)]).unwrap()),
            [(2, 20), (1, 10)]
        );
        assert_eq!(
            TestMap::try_from(vec![(2, 20), (1, 10), (2, 99)]),
            Err(BoundedError::DuplicateItem { index: 2 })
        );
        assert_eq!(TestMap::try_from(vec![]), Err(BoundedError::EmptyInput));
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
        assert!(UpperBoundedOrderedMap::<u8, u16, 4>::empty().is_empty());
        assert!(UpperBoundedOrderedMap::<u8, u16, 4>::default().is_empty());
    }

    #[test]
    fn try_push_appends_a_new_entry_at_the_end() {
        let mut map = TestMap::try_from_iter([(3, 30), (1, 10)]).unwrap();

        assert_eq!(map.try_insert(2, 20), Ok(()));
        assert_eq!(entries(&map), [(3, 30), (1, 10), (2, 20)]);
    }

    #[test]
    fn try_push_rejects_a_repeated_key_and_does_not_mutate() {
        let mut map = TestMap::try_from_iter([(3, 30), (1, 10)]).unwrap();

        assert_eq!(
            map.try_insert(3, 99),
            Err(BoundedError::DuplicateItem { index: 2 })
        );
        assert_eq!(entries(&map), [(3, 30), (1, 10)]);
    }

    #[test]
    fn try_push_rejects_growth_past_max() {
        let mut map = TestMap::try_from_iter([(4, 40), (3, 30), (2, 20), (1, 10)]).unwrap();

        assert_eq!(
            map.try_insert(5, 50),
            Err(BoundedError::TooManyItems { count: 5, max: 4 })
        );
        assert_eq!(entries(&map), [(4, 40), (3, 30), (2, 20), (1, 10)]);
    }

    #[test]
    fn try_pop_returns_none_at_or_below_lower_bound() {
        let mut map = TestMap::try_from_iter([(1, 10), (2, 20), (3, 30)]).unwrap();

        assert_eq!(map.try_pop(), Ok(Some((3, 30))));
        assert_eq!(map.try_pop(), Ok(None));
        assert_eq!(entries(&map), [(1, 10), (2, 20)]);
    }

    #[test]
    fn try_remove_removes_the_entry_at_index_and_shifts_the_rest() {
        let mut map = TestMap::try_from_iter([(4, 40), (3, 30), (2, 20), (1, 10)]).unwrap();

        assert_eq!(map.try_remove(1), Ok((3, 30)));
        assert_eq!(entries(&map), [(4, 40), (2, 20), (1, 10)]);
        assert!(!map.contains_key(&3));
    }

    #[test]
    fn try_remove_rejects_removal_below_min_and_out_of_bounds() {
        let mut map = TestMap::try_from_iter([(1, 10), (2, 20)]).unwrap();

        assert_eq!(
            map.try_remove(0),
            Err(BoundedError::TooFewItems { count: 1, min: 2 })
        );
        assert_eq!(
            map.try_remove(2),
            Err(BoundedError::IndexOutOfBounds { index: 2, len: 2 })
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
    fn into_iterator_and_into_vec_follow_insertion_order() {
        let map = TestMap::try_from_iter([(2, 20), (1, 10)]).unwrap();

        assert_eq!(
            (&map)
                .into_iter()
                .map(|(k, v)| (*k, *v))
                .collect::<Vec<_>>(),
            [(2, 20), (1, 10)]
        );
        assert_eq!(Vec::from(map), [(2, 20), (1, 10)]);
    }

    #[test]
    fn equality_hashing_and_ordering_follow_the_sequence() {
        let forward = TestMap::try_from_iter([(1, 10), (2, 20)]).unwrap();
        let same = TestMap::try_from_iter([(1, 10), (2, 20)]).unwrap();
        let backward = TestMap::try_from_iter([(2, 20), (1, 10)]).unwrap();

        assert_eq!(forward, same);
        assert_ne!(forward, backward);
        assert!(forward < backward);
        // The wrapped `IndexMap` compares as a map and still calls them equal.
        assert_eq!(**forward.as_inner(), **backward.as_inner());

        let state = RandomState::new();
        assert_eq!(state.hash_one(&forward), state.hash_one(&same));
        let distinct: HashSet<TestMap> = [forward, same, backward].into_iter().collect();
        assert_eq!(distinct.len(), 2);
    }

    #[test]
    fn json_roundtrip_preserves_order() {
        let original = TestMap::try_from_iter([(3, 30), (1, 10), (2, 20)]).unwrap();

        let json = serde_json::to_string(&original).unwrap();
        let restored: TestMap = serde_json::from_str(&json).unwrap();

        assert_eq!(json, r#"{"3":30,"1":10,"2":20}"#);
        assert_eq!(restored, original);
    }

    #[test]
    fn binary_roundtrip_preserves_order_and_the_map_wire_format() {
        let original = TestMap::try_from_iter([(3, 30), (1, 10), (2, 20)]).unwrap();

        let encoded = bincode::serialize(&original).unwrap();
        let restored = bincode::deserialize::<TestMap>(&encoded).unwrap();

        assert_eq!(restored, original);
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

    /// Once its key proves an entry cannot be admitted, its value is not worth
    /// decoding: a repeated key costs no more than the key itself.
    #[test]
    fn deserialize_rejects_a_repeated_key_before_decoding_its_value() {
        let (json_error, json_attempts) =
            error_and_value_attempts(r#"{"1":1,"2":2,"1":3}"#, from_json);
        assert!(
            json_error.contains("Item at index 2 is a duplicate of an earlier item"),
            "unexpected error: {json_error}"
        );
        assert_eq!(json_attempts, 2, "only the two admitted values are decoded");

        let encoded = bincode::serialize(&vec![(1u8, 1u16), (2, 2), (1, 3)]).unwrap();
        let (binary_error, binary_attempts) = error_and_value_attempts(&encoded[..], from_bincode);
        assert!(
            binary_error.contains("duplicate"),
            "unexpected error: {binary_error}"
        );
        assert_eq!(
            binary_attempts, 2,
            "only the two admitted values are decoded"
        );
    }

    /// The repeated key is the first thing wrong with the input, so it is what
    /// gets reported, whatever follows it.
    #[test]
    fn deserialize_reports_a_repeated_key_even_when_its_value_is_malformed() {
        let malformed =
            serde_json::from_str::<TestMap>(r#"{"1":10,"2":20,"1":"malformed"}"#).unwrap_err();
        assert!(
            malformed
                .to_string()
                .contains("Item at index 2 is a duplicate of an earlier item"),
            "unexpected error: {malformed}"
        );

        // Declares two entries, then ends right after repeating the first key.
        let mut truncated = bincode::serialize(&vec![(1u8, 10u16)]).unwrap();
        truncated[0] = 2;
        truncated.push(1);
        let missing = bincode::deserialize::<TestMap>(&truncated).unwrap_err();
        assert!(
            missing.to_string().contains("duplicate"),
            "unexpected error: {missing}"
        );
    }

    /// An entry past `MAX` is refused on its key, like a repeated one.
    #[test]
    fn deserialize_stops_before_the_value_of_an_entry_past_maximum() {
        let (error, attempts) =
            error_and_value_attempts(r#"{"1":1,"2":2,"3":3,"4":4,"5":5}"#, from_json);

        assert!(
            error.contains("exceeds static maximum"),
            "unexpected error: {error}"
        );
        assert_eq!(attempts, 4, "only the four admitted values are decoded");
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
        type Unbounded = BoundedOrderedMap<u8, u8, 0, { usize::MAX }>;

        let result = bincode::deserialize::<Unbounded>(&u64::MAX.to_le_bytes());

        assert!(result.is_err());
    }
}
