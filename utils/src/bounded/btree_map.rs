use core::{borrow::Borrow, ops::Deref};
use std::collections::{BTreeMap, btree_map};

use serde::{Deserialize, Deserializer};

use crate::bounded::{
    Bounded, BoundedError, BoundedLen,
    collection::{self, BoundedCollection, MapVisitor},
};

impl<K, V> BoundedLen for BTreeMap<K, V> {
    fn bounded_len(&self) -> usize {
        self.len()
    }
}

impl<K, V> BoundedCollection for BTreeMap<K, V>
where
    K: Ord,
{
    type Item = (K, V);

    // A B-tree allocates node by node as it grows, so there is nothing to
    // reserve up front.
    fn with_capacity(_capacity: usize) -> Self {
        Self::new()
    }

    fn add(&mut self, (key, value): (K, V)) -> bool {
        self.insert(key, value).is_none()
    }
}

/// A [`BTreeMap`] whose entry count is statically enforced to be in the range
/// `[MIN, MAX]`.
///
/// A thin alias over [`Bounded`]. The entries are always held in increasing
/// key order, whatever order they were given in, so two maps with the same
/// entries are equal, hash the same, and serialize the same.
///
/// Every checked construction path ([`TryFrom<Vec<(K, V)>>`](TryFrom),
/// [`Self::try_from_iter`]) enforces the bound and rejects a repeated key
/// instead of letting the later entry overwrite the earlier one.
/// Deserialization reads at most `MAX` entries, repeats included, refusing the
/// one past `MAX` on its key; the entries may come in any order, and a
/// repeated key takes its last value, as it does for a [`BTreeMap`]. `MIN`
/// applies to the entries held once repeated keys have merged. Entries are
/// added with
/// [`Self::try_insert`] and removed with [`Self::try_remove`]. Values can be
/// changed in place; keys cannot.
///
/// Read access goes through `Deref` to the inner [`BTreeMap`]. There is no
/// `DerefMut`: a mutable [`BTreeMap`] could change the length past the bound.
pub type BoundedBTreeMap<K, V, const MIN: usize, const MAX: usize> =
    Bounded<BTreeMap<K, V>, MIN, MAX>;
/// A bounded B-tree map containing between zero and `MAX` entries.
pub type UpperBoundedBTreeMap<K, V, const MAX: usize> = BoundedBTreeMap<K, V, 0, MAX>;
/// A non-empty bounded B-tree map containing at most `MAX` entries.
pub type NonEmptyBoundedBTreeMap<K, V, const MAX: usize> = BoundedBTreeMap<K, V, 1, MAX>;

impl<K, V, const MIN: usize, const MAX: usize> BoundedBTreeMap<K, V, MIN, MAX> {
    /// Constructs an empty map.
    ///
    /// Only valid when `MIN` is zero: any other `MIN` fails to compile.
    #[must_use]
    pub const fn empty() -> Self {
        const {
            assert!(
                MIN == 0,
                "Cannot construct empty BoundedBTreeMap when MIN > 0"
            );
        }
        Self::new_unchecked(BTreeMap::new())
    }

    /// Returns an iterator over the entries in key order, with mutable
    /// references to the values.
    pub fn iter_mut(&mut self) -> btree_map::IterMut<'_, K, V> {
        self.0.iter_mut()
    }

    /// Returns an iterator over mutable references to the values, in key
    /// order.
    pub fn values_mut(&mut self) -> btree_map::ValuesMut<'_, K, V> {
        self.0.values_mut()
    }
}

impl<K, V, const MIN: usize, const MAX: usize> BoundedBTreeMap<K, V, MIN, MAX>
where
    K: Ord,
{
    /// Constructs a bounded B-tree map from an iterable of entries with
    /// distinct keys, in any order.
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

    /// Inserts the entry `key => value` if `key` is new and doing so does not
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
            btree_map::Entry::Occupied(_) => Err(BoundedError::DuplicateItem { index: len }),
            btree_map::Entry::Vacant(entry) => {
                entry.insert(value);
                Ok(())
            }
        }
    }

    /// Removes the entry under `key` and returns its value, if the minimum
    /// length is kept.
    ///
    /// Returns `Ok(None)` when `key` is absent, and
    /// [`BoundedError::TooFewItems`] when removing the entry would violate
    /// `MIN`; the map is left untouched either way.
    pub fn try_remove<Q>(&mut self, key: &Q) -> Result<Option<V>, BoundedError>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if !self.contains_key(key) {
            return Ok(None);
        }
        let new_len = self.len() - 1;
        if new_len < MIN {
            return Err(BoundedError::TooFewItems {
                count: new_len,
                min: MIN,
            });
        }
        Ok(self.0.remove(key))
    }

    /// Returns a mutable reference to the value under `key`.
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.0.get_mut(key)
    }
}

impl<K, V, const MIN: usize, const MAX: usize> Default for BoundedBTreeMap<K, V, MIN, MAX> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<K, V, const MIN: usize, const MAX: usize> TryFrom<Vec<(K, V)>>
    for BoundedBTreeMap<K, V, MIN, MAX>
where
    K: Ord,
{
    type Error = BoundedError;

    fn try_from(value: Vec<(K, V)>) -> Result<Self, Self::Error> {
        Self::try_from_iter(value)
    }
}

impl<K, V, const MIN: usize, const MAX: usize> TryFrom<BTreeMap<K, V>>
    for BoundedBTreeMap<K, V, MIN, MAX>
{
    type Error = BoundedError;

    fn try_from(value: BTreeMap<K, V>) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl<K, V, const MIN: usize, const MAX: usize> From<BoundedBTreeMap<K, V, MIN, MAX>>
    for BTreeMap<K, V>
{
    fn from(value: BoundedBTreeMap<K, V, MIN, MAX>) -> Self {
        value.into_inner()
    }
}

impl<K, V, const MIN: usize, const MAX: usize> From<BoundedBTreeMap<K, V, MIN, MAX>>
    for Vec<(K, V)>
{
    fn from(value: BoundedBTreeMap<K, V, MIN, MAX>) -> Self {
        value.into_iter().collect()
    }
}

impl<K, V, const MIN: usize, const MAX: usize> Deref for BoundedBTreeMap<K, V, MIN, MAX> {
    type Target = BTreeMap<K, V>;

    fn deref(&self) -> &Self::Target {
        self.as_inner()
    }
}

impl<K, V, const MIN: usize, const MAX: usize> From<(K, V)> for BoundedBTreeMap<K, V, MIN, MAX>
where
    K: Ord,
{
    fn from(value: (K, V)) -> Self {
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
        Self::new_unchecked(BTreeMap::from([value]))
    }
}

impl<K, V, const MIN: usize, const MAX: usize, const INPUT_SIZE: usize>
    TryFrom<[(K, V); INPUT_SIZE]> for BoundedBTreeMap<K, V, MIN, MAX>
where
    K: Ord,
{
    type Error = BoundedError;

    fn try_from(value: [(K, V); INPUT_SIZE]) -> Result<Self, Self::Error> {
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

impl<'a, K, V, const MIN: usize, const MAX: usize> IntoIterator
    for &'a BoundedBTreeMap<K, V, MIN, MAX>
{
    type Item = (&'a K, &'a V);
    type IntoIter = btree_map::Iter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, K, V, const MIN: usize, const MAX: usize> IntoIterator
    for &'a mut BoundedBTreeMap<K, V, MIN, MAX>
{
    type Item = (&'a K, &'a mut V);
    type IntoIter = btree_map::IterMut<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl<K, V, const MIN: usize, const MAX: usize> IntoIterator for BoundedBTreeMap<K, V, MIN, MAX> {
    type Item = (K, V);
    type IntoIter = btree_map::IntoIter<K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_inner().into_iter()
    }
}

impl<'de, K, V, const MIN: usize, const MAX: usize> Deserialize<'de>
    for BoundedBTreeMap<K, V, MIN, MAX>
where
    K: Deserialize<'de> + Ord,
    V: Deserialize<'de>,
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
        collections::{BTreeMap, HashSet},
        hash::{BuildHasher as _, RandomState},
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use serde::{Deserialize, Deserializer};

    use crate::bounded::{
        BoundedBTreeMap, BoundedError, UpperBoundedBTreeMap,
        collection::test_utils::assert_serde_matches_underlying,
    };

    /// Concrete instantiation used across the tests: between 2 and 4 entries.
    type TestMap = BoundedBTreeMap<u8, u16, 2, 4>;

    /// Like [`TestMap`], but its values count how often they are decoded.
    type CountingMap = BoundedBTreeMap<u8, CountingValue, 0, 4>;

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

    /// `entries` as a JSON object, in order and with any repeated key kept.
    fn json_object(entries: &[(u8, u16)]) -> String {
        let fields: Vec<String> = entries
            .iter()
            .map(|(key, value)| format!(r#""{key}":{value}"#))
            .collect();
        format!("{{{}}}", fields.join(","))
    }

    #[test]
    fn try_from_iter_sorts_the_entries_by_key() {
        let map = TestMap::try_from_iter([(3, 30), (1, 10), (2, 20)]).unwrap();

        assert_eq!(entries(&map), [(1, 10), (2, 20), (3, 30)]);
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
    fn try_from_vec_and_array_check_uniqueness_and_bounds() {
        assert_eq!(
            entries(&TestMap::try_from(vec![(2, 20), (1, 10)]).unwrap()),
            [(1, 10), (2, 20)]
        );
        assert_eq!(
            TestMap::try_from(vec![(2, 20), (1, 10), (2, 99)]),
            Err(BoundedError::DuplicateItem { index: 2 })
        );
        assert_eq!(TestMap::try_from(vec![]), Err(BoundedError::EmptyInput));
        assert_eq!(
            TestMap::try_from([(2, 20), (2, 99)]),
            Err(BoundedError::DuplicateItem { index: 1 })
        );
    }

    #[test]
    fn try_from_btree_map_checks_only_the_length() {
        let inner = BTreeMap::from([(2, 20), (1, 10)]);

        let map = TestMap::try_from(inner).unwrap();

        assert_eq!(entries(&map), [(1, 10), (2, 20)]);
        assert_eq!(
            TestMap::try_from(BTreeMap::new()),
            Err(BoundedError::EmptyInput)
        );
    }

    #[test]
    fn empty_default_and_single_entry_construction() {
        assert!(UpperBoundedBTreeMap::<u8, u16, 4>::empty().is_empty());
        assert!(UpperBoundedBTreeMap::<u8, u16, 4>::default().is_empty());

        let single = UpperBoundedBTreeMap::<u8, u16, 4>::from((7, 70));
        assert_eq!(single.iter().collect::<Vec<_>>(), [(&7, &70)]);
    }

    #[test]
    fn try_insert_keeps_the_entries_in_key_order() {
        let mut map = TestMap::try_from_iter([(3, 30), (1, 10)]).unwrap();

        assert_eq!(map.try_insert(2, 20), Ok(()));
        assert_eq!(entries(&map), [(1, 10), (2, 20), (3, 30)]);
    }

    #[test]
    fn try_insert_rejects_a_repeated_key_and_does_not_mutate() {
        let mut map = TestMap::try_from_iter([(3, 30), (1, 10)]).unwrap();

        assert_eq!(
            map.try_insert(3, 99),
            Err(BoundedError::DuplicateItem { index: 2 })
        );
        assert_eq!(entries(&map), [(1, 10), (3, 30)]);
    }

    #[test]
    fn try_insert_rejects_growth_past_max() {
        let mut map = TestMap::try_from_iter([(4, 40), (3, 30), (2, 20), (1, 10)]).unwrap();

        assert_eq!(
            map.try_insert(5, 50),
            Err(BoundedError::TooManyItems { count: 5, max: 4 })
        );
        assert_eq!(entries(&map), [(1, 10), (2, 20), (3, 30), (4, 40)]);
    }

    #[test]
    fn try_remove_removes_the_entry_under_a_key() {
        let mut map = TestMap::try_from_iter([(1, 10), (2, 20), (3, 30)]).unwrap();

        assert_eq!(map.try_remove(&2), Ok(Some(20)));
        assert_eq!(map.try_remove(&9), Ok(None));
        assert_eq!(entries(&map), [(1, 10), (3, 30)]);
    }

    #[test]
    fn try_remove_rejects_removal_below_min() {
        let mut map = TestMap::try_from_iter([(1, 10), (2, 20)]).unwrap();

        assert_eq!(
            map.try_remove(&1),
            Err(BoundedError::TooFewItems { count: 1, min: 2 })
        );
        // An absent key removes nothing, so it cannot break the bound.
        assert_eq!(map.try_remove(&9), Ok(None));
        assert_eq!(entries(&map), [(1, 10), (2, 20)]);
    }

    #[test]
    fn values_can_be_mutated_in_place() {
        let mut map = TestMap::try_from_iter([(2, 20), (1, 10)]).unwrap();

        *map.get_mut(&1).unwrap() += 1;
        for value in map.values_mut() {
            *value *= 10;
        }
        for (_, value) in &mut map {
            *value += 1;
        }

        assert_eq!(entries(&map), [(1, 111), (2, 201)]);
    }

    #[test]
    fn into_iterator_and_into_vec_follow_key_order() {
        let map = TestMap::try_from_iter([(2, 20), (1, 10)]).unwrap();

        assert_eq!(
            (&map)
                .into_iter()
                .map(|(k, v)| (*k, *v))
                .collect::<Vec<_>>(),
            [(1, 10), (2, 20)]
        );
        assert_eq!(Vec::from(map), [(1, 10), (2, 20)]);
    }

    #[test]
    fn equality_and_hashing_ignore_the_construction_order() {
        let forward = TestMap::try_from_iter([(1, 10), (2, 20)]).unwrap();
        let backward = TestMap::try_from_iter([(2, 20), (1, 10)]).unwrap();
        let other = TestMap::try_from_iter([(1, 10), (2, 21)]).unwrap();

        assert_eq!(forward, backward);
        assert_ne!(forward, other);

        let state = RandomState::new();
        assert_eq!(state.hash_one(&forward), state.hash_one(&backward));
        let distinct: HashSet<TestMap> = [forward, backward, other].into_iter().collect();
        assert_eq!(distinct.len(), 2);
    }

    #[test]
    fn json_roundtrip_writes_the_keys_in_order() {
        let original = TestMap::try_from_iter([(3, 30), (1, 10), (2, 20)]).unwrap();

        let json = serde_json::to_string(&original).unwrap();
        let restored: TestMap = serde_json::from_str(&json).unwrap();

        assert_eq!(json, r#"{"1":10,"2":20,"3":30}"#);
        assert_eq!(restored, original);
    }

    #[test]
    fn binary_roundtrip_uses_the_map_wire_format_in_key_order() {
        let original = TestMap::try_from_iter([(3, 30), (1, 10), (2, 20)]).unwrap();

        let encoded = bincode::serialize(&original).unwrap();
        let restored = bincode::deserialize::<TestMap>(&encoded).unwrap();

        assert_eq!(restored, original);
        assert_eq!(
            encoded,
            bincode::serialize(&vec![(1u8, 10u16), (2, 20), (3, 30)]).unwrap()
        );
    }

    /// Within its bounds, a bounded B-tree map reads and writes exactly as a
    /// B-tree map does: the entries may come in any order, and a repeated key
    /// takes its last value.
    #[test]
    fn serde_matches_the_underlying_btree_map() {
        for entries in [
            &[(2u8, 20u16), (1, 10)][..],
            &[(3, 30), (1, 10), (3, 99)],
            &[(1, 1), (2, 2), (1, 3), (2, 4)],
        ] {
            assert_serde_matches_underlying::<TestMap, BTreeMap<u8, u16>>(
                &json_object(entries),
                &bincode::serialize(entries).unwrap(),
            );
        }
    }

    /// Every entry read counts against `MAX`, repeated keys included, so the
    /// work an input costs stays bounded. The entry past `MAX` is refused on
    /// its key, so its value is never decoded.
    #[test]
    fn deserialize_refuses_the_entry_past_maximum_before_decoding_its_value() {
        let (json_error, json_attempts) =
            error_and_value_attempts(r#"{"1":1,"1":2,"1":3,"1":4,"1":5}"#, from_json);
        assert!(
            json_error.contains("Item count 5 exceeds static maximum of 4"),
            "unexpected error: {json_error}"
        );
        assert_eq!(
            json_attempts, 4,
            "the value of the refused entry is not decoded"
        );

        // A declared length past `MAX` is refused before any entry is decoded.
        let encoded = bincode::serialize(&vec![(1u8, 1u16); 5]).unwrap();
        let (binary_error, binary_attempts) = error_and_value_attempts(&encoded[..], from_bincode);
        assert!(
            binary_error.contains("Item count 5 exceeds static maximum of 4"),
            "unexpected error: {binary_error}"
        );
        assert_eq!(binary_attempts, 0, "no entry is decoded");
    }

    /// The minimum applies to the entries held, once repeated keys have
    /// merged.
    #[test]
    fn deserialize_checks_the_minimum_after_merging() {
        let err = serde_json::from_str::<TestMap>(r#"{"1":10,"1":20}"#).unwrap_err();

        assert!(
            err.to_string()
                .contains("Item count 1 is below minimum of 2"),
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

        let encoded =
            bincode::serialize(&vec![(1u8, 1u16), (2, 2), (3, 3), (4, 4), (5, 5)]).unwrap();
        let too_many = bincode::deserialize::<TestMap>(&encoded).unwrap_err();
        assert!(
            too_many.to_string().contains("exceeds static maximum"),
            "unexpected error: {too_many}"
        );
    }

    /// A declared length of `u64::MAX` entries, from an 8-byte input, fails
    /// on the missing entries rather than on an allocation.
    #[test]
    fn deserialize_binary_does_not_preallocate_a_huge_declared_length() {
        type Unbounded = BoundedBTreeMap<u8, u8, 0, { usize::MAX }>;

        let result = bincode::deserialize::<Unbounded>(&u64::MAX.to_le_bytes());

        assert!(result.is_err());
    }
}
