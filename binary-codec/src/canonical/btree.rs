//! Canonical codecs for [`BTreeSet`] and [`BTreeMap`], and for their bounded
//! variants [`BoundedBTreeSet`] and [`BoundedBTreeMap`].
//!
//! All four share one format: a length prefix, then the entries in increasing
//! key order, a set's entry being its element and a map's its key followed by
//! its value. That is the collections' own order, so equal collections encode
//! alike however they were built. A bounded collection takes the prefix its
//! `MAX` needs. An unbounded one takes the widest there is, so one of more than
//! `u32::MAX` entries cannot be encoded: it is coded as a bounded one with
//! `MIN = 0` and `MAX = MAX_ENCODABLE_LENGTH`, by the same functions.
//!
//! Decoding accepts that order and nothing else, so that a collection has
//! exactly one encoding: were any order accepted, one of `n` entries would have
//! `n!`. Each key must come after the key before it, and one that does not is
//! rejected where it appears, before the rest of its entry is decoded: as a
//! duplicate when it repeats the key before it, and as out of order otherwise.
//! That also bounds a key type that decodes without consuming input: its second
//! key repeats the first, so no length prefix can make the decode loop spin.

use core::{any::type_name, cmp::Ordering};
use std::borrow::Cow;

use lb_utils::bounded::{BoundedBTreeMap, BoundedBTreeSet};

use crate::canonical::{
    BinaryDecode, BinaryEncode, CodecExamples, CodecFixture, CodecFixtures, DecodeError,
    fixtures::{cycled_fixture, distinct_fixtures_in_declared_order},
    length_prefix::{
        MAX_ENCODABLE_LENGTH, decode_bounded_length, encode_length_prefix_into, length_prefix_len,
    },
    sealed,
};

pub type BTreeSet<T> = BoundedBTreeSet<T, 0, MAX_ENCODABLE_LENGTH>;

impl<T, const MIN: usize, const MAX: usize> BinaryEncode for BoundedBTreeSet<T, MIN, MAX>
where
    T: BinaryEncode + Ord,
{
    fn encoded_length(&self) -> usize {
        set_encoded_length::<T, MAX>(self)
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        encode_set_into::<T, MAX>(self, out);
    }
}

impl<T, const MIN: usize, const MAX: usize> BinaryDecode for BoundedBTreeSet<T, MIN, MAX>
where
    T: BinaryDecode + Ord,
{
    type Context = T::Context;

    fn decode<'input>(
        input: &'input [u8],
        context: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (rest, set) = decode_set::<Self, T, MIN, MAX>(input, context)?;
        Ok((rest, Self::new_unchecked(set)))
    }
}

impl<T, const MIN: usize, const MAX: usize> sealed::Sealed for BoundedBTreeSet<T, MIN, MAX> where
    T: CodecExamples
{
}

impl<T, const MIN: usize, const MAX: usize> CodecExamples for BoundedBTreeSet<T, MIN, MAX>
where
    T: CodecExamples + Ord,
{
    fn fixtures() -> CodecFixtures<Self> {
        into_codec_fixtures(set_fixtures::<Self, T, MIN, MAX>(), Self::new_unchecked)
    }
}

pub type BTreeMap<K, V> = BoundedBTreeMap<K, V, 0, MAX_ENCODABLE_LENGTH>;

impl<K, V, const MIN: usize, const MAX: usize> BinaryEncode for BoundedBTreeMap<K, V, MIN, MAX>
where
    K: BinaryEncode + Ord,
    V: BinaryEncode,
{
    fn encoded_length(&self) -> usize {
        map_encoded_length::<K, V, MAX>(self)
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        encode_map_into::<K, V, MAX>(self, out);
    }
}

impl<K, V, const MIN: usize, const MAX: usize> BinaryDecode for BoundedBTreeMap<K, V, MIN, MAX>
where
    K: BinaryDecode + Ord,
    V: BinaryDecode,
{
    type Context = (K::Context, V::Context);

    fn decode<'input>(
        input: &'input [u8],
        context: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (rest, map) = decode_map::<Self, K, V, MIN, MAX>(input, context)?;
        Ok((rest, Self::new_unchecked(map)))
    }
}

impl<K, V, const MIN: usize, const MAX: usize> sealed::Sealed for BoundedBTreeMap<K, V, MIN, MAX>
where
    K: CodecExamples,
    V: CodecExamples,
{
}

impl<K, V, const MIN: usize, const MAX: usize> CodecExamples for BoundedBTreeMap<K, V, MIN, MAX>
where
    K: CodecExamples + Ord,
    V: CodecExamples,
{
    fn fixtures() -> CodecFixtures<Self> {
        into_codec_fixtures(map_fixtures::<Self, K, V, MIN, MAX>(), Self::new_unchecked)
    }
}

fn set_encoded_length<T, const MAX: usize>(set: &std::collections::BTreeSet<T>) -> usize
where
    T: BinaryEncode,
{
    with_prefix_length::<MAX>(set.iter().map(BinaryEncode::encoded_length).sum())
}

fn encode_set_into<T, const MAX: usize>(set: &std::collections::BTreeSet<T>, out: &mut Vec<u8>)
where
    T: BinaryEncode,
{
    encode_length_prefix_into::<MAX>(set.len(), out);
    for element in set {
        element.encode_into(out);
    }
}

/// Decodes a set of between `MIN` and `MAX` elements, naming `Owner` in its
/// errors.
fn decode_set<'input, Owner, T, const MIN: usize, const MAX: usize>(
    input: &'input [u8],
    context: &T::Context,
) -> Result<(&'input [u8], std::collections::BTreeSet<T>), DecodeError>
where
    Owner: ?Sized,
    T: BinaryDecode + Ord,
{
    let (mut rest, len) = decode_bounded_length::<Owner, MIN, MAX>(input)?;

    let mut set = std::collections::BTreeSet::new();
    for index in 0..len {
        let (next, element) = T::decode(rest, context)?;
        check_key_order::<Owner, T>(set.last(), &element, index)?;
        set.insert(element);
        rest = next;
    }
    Ok((rest, set))
}

fn map_encoded_length<K, V, const MAX: usize>(map: &std::collections::BTreeMap<K, V>) -> usize
where
    K: BinaryEncode,
    V: BinaryEncode,
{
    with_prefix_length::<MAX>(
        map.iter()
            .map(|(key, value)| key.encoded_length() + value.encoded_length())
            .sum(),
    )
}

fn encode_map_into<K, V, const MAX: usize>(
    map: &std::collections::BTreeMap<K, V>,
    out: &mut Vec<u8>,
) where
    K: BinaryEncode,
    V: BinaryEncode,
{
    encode_length_prefix_into::<MAX>(map.len(), out);
    for (key, value) in map {
        key.encode_into(out);
        value.encode_into(out);
    }
}

/// Decodes a map of between `MIN` and `MAX` entries, naming `Owner` in its
/// errors.
fn decode_map<'input, Owner, K, V, const MIN: usize, const MAX: usize>(
    input: &'input [u8],
    context: &(K::Context, V::Context),
) -> Result<(&'input [u8], std::collections::BTreeMap<K, V>), DecodeError>
where
    Owner: ?Sized,
    K: BinaryDecode + Ord,
    V: BinaryDecode,
{
    let (mut rest, len) = decode_bounded_length::<Owner, MIN, MAX>(input)?;

    let mut map = std::collections::BTreeMap::new();
    for index in 0..len {
        let (after_key, key) = K::decode(rest, &context.0)?;
        // Checked before the value is decoded, so a misplaced key costs
        // nothing more.
        check_key_order::<Owner, K>(map.last_key_value().map(|(last, _)| last), &key, index)?;
        let (next, value) = V::decode(after_key, &context.1)?;
        map.insert(key, value);
        rest = next;
    }
    Ok((rest, map))
}

/// The length of a collection whose entries take `entries` bytes, with its
/// `MAX`-width length prefix.
fn with_prefix_length<const MAX: usize>(entries: usize) -> usize {
    length_prefix_len::<MAX>()
        .checked_add(entries)
        .expect("Encoded length overflow")
}

/// Refuses the key at input position `index` unless it comes after `last`,
/// the key before it: equal to it, the key is a repeat, and smaller, it is out
/// of order.
fn check_key_order<Owner, K>(last: Option<&K>, key: &K, index: usize) -> Result<(), DecodeError>
where
    Owner: ?Sized,
    K: Ord,
{
    match last.map(|last| key.cmp(last)) {
        None | Some(Ordering::Greater) => Ok(()),
        Some(Ordering::Equal) => Err(DecodeError::duplicate_item::<Owner>(index)),
        Some(Ordering::Less) => Err(DecodeError::out_of_order_item::<Owner>(index)),
    }
}

// A collection's fixtures are the empty collection, when its bound allows one,
// and the collection holding as many of the key type's distinct fixtures as the
// bound allows, in the order they encode in. A map's values cycle through the
// value type's fixtures, so any value type works.

fn set_fixtures<Owner, T, const MIN: usize, const MAX: usize>()
-> Vec<(std::collections::BTreeSet<T>, Vec<u8>)>
where
    T: CodecExamples + Ord,
{
    let mut fixtures = empty_fixture::<_, MIN, MAX>(std::collections::BTreeSet::new);
    let elements = sorted_distinct_fixtures::<Owner, T, MIN, MAX>();
    if !elements.is_empty() {
        let mut bytes = Vec::new();
        encode_length_prefix_into::<MAX>(elements.len(), &mut bytes);
        let mut set = std::collections::BTreeSet::new();
        for element in elements {
            bytes.extend_from_slice(element.bytes.as_ref());
            assert!(
                set.insert(element.value),
                "two fixtures of {} with different bytes compare equal",
                type_name::<T>(),
            );
        }
        fixtures.push((set, bytes));
    }
    fixtures
}

fn map_fixtures<Owner, K, V, const MIN: usize, const MAX: usize>()
-> Vec<(std::collections::BTreeMap<K, V>, Vec<u8>)>
where
    K: CodecExamples + Ord,
    V: CodecExamples,
{
    let mut fixtures = empty_fixture::<_, MIN, MAX>(std::collections::BTreeMap::new);
    let keys = sorted_distinct_fixtures::<Owner, K, MIN, MAX>();
    if !keys.is_empty() {
        let mut bytes = Vec::new();
        encode_length_prefix_into::<MAX>(keys.len(), &mut bytes);
        let mut map = std::collections::BTreeMap::new();
        for (index, key) in keys.into_iter().enumerate() {
            let value = cycled_fixture::<V>(index);
            bytes.extend_from_slice(key.bytes.as_ref());
            bytes.extend_from_slice(value.bytes.as_ref());
            assert!(
                map.insert(key.value, value.value).is_none(),
                "two fixtures of {} with different bytes compare equal",
                type_name::<K>(),
            );
        }
        fixtures.push((map, bytes));
    }
    fixtures
}

/// The empty collection with its bytes, if `MIN` allows it.
fn empty_fixture<Collection, const MIN: usize, const MAX: usize>(
    empty: impl FnOnce() -> Collection,
) -> Vec<(Collection, Vec<u8>)> {
    if MIN > 0 {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    encode_length_prefix_into::<MAX>(0, &mut bytes);
    vec![(empty(), bytes)]
}

/// Up to `MAX` distinct fixtures of `K`, and at least `MIN`, in increasing
/// order: the keys of a collection's fixture, in the order it encodes them.
fn sorted_distinct_fixtures<Owner, K, const MIN: usize, const MAX: usize>() -> Vec<CodecFixture<K>>
where
    K: CodecExamples + Ord,
{
    let mut keys = distinct_fixtures_in_declared_order::<Owner, K, MIN, MAX>();
    keys.sort_by(|left, right| left.value.cmp(&right.value));
    keys
}

/// `fixtures`, each collection wrapped by `wrap`, as a codec's fixtures.
fn into_codec_fixtures<Collection, Wrapped>(
    fixtures: Vec<(Collection, Vec<u8>)>,
    wrap: impl Fn(Collection) -> Wrapped,
) -> CodecFixtures<Wrapped> {
    fixtures
        .into_iter()
        .map(|(collection, bytes)| CodecFixture {
            value: wrap(collection),
            bytes: Cow::Owned(bytes),
        })
        .collect::<Vec<_>>()
        .try_into()
        .expect("a bound either allows the empty collection or needs a populated one")
}

#[cfg(test)]
mod tests {
    use lb_utils::bounded::{BoundedBTreeMap, BoundedBTreeSet};

    use crate::canonical::{
        BinaryDecode as _, BinaryEncode as _, CodecExamples as _, DecodeError,
        assert_codec_fixtures, assert_codec_fixtures_with,
        btree::{BTreeMap, BTreeSet},
        tests::allocation::bytes_allocated_by,
    };

    /// Bounds used across the tests: between 2 and 4 entries.
    type Set = BoundedBTreeSet<u16, 2, 4>;
    type Map = BoundedBTreeMap<u16, u8, 2, 4>;

    #[test]
    fn entries_follow_a_four_byte_length_in_ascending_key_order() {
        // Inserted out of order: the encoding follows the keys.
        let map = BTreeMap::try_from([(2u8, 20u8), (1u8, 10u8)]).unwrap();
        assert_eq!(map.encode_to_vec(), [2, 0, 0, 0, 1, 10, 2, 20]);

        let set = BTreeSet::from(2u8);
        assert_eq!(set.encode_to_vec(), [2, 0, 0, 0, 1, 2]);
    }

    #[test]
    fn bounded_collections_take_the_prefix_their_maximum_needs() {
        // Count `02`, then `0100` and `0001`, whatever order the entries were
        // given in.
        let set = Set::try_from_iter([256, 1]).unwrap();
        assert_eq!(hex::encode(set.encode()), "0201000001");

        let map = Map::try_from_iter([(256, 0xBB), (1, 0xAA)]).unwrap();
        assert_eq!(hex::encode(map.encode()), "020100aa0001bb");
    }

    #[test]
    fn decode_accepts_keys_in_increasing_order() {
        let (rest, set) = BTreeSet::<u8>::decode(&[2, 0, 0, 0, 1, 2], &()).unwrap();
        assert!(rest.is_empty());
        assert_eq!(set, BTreeSet::try_from_iter([1, 2]).unwrap());

        let bytes = [2, 0x01, 0x00, 0xAA, 0x00, 0x01, 0xBB];
        let (rest, map) = Map::decode(&bytes, &((), ())).unwrap();
        assert!(rest.is_empty());
        assert_eq!(map.encode_to_vec(), bytes);
    }

    #[test]
    fn decode_rejects_a_repeated_key() {
        let set = BTreeSet::<u8>::decode(&[2, 0, 0, 0, 1, 1], &()).unwrap_err();
        assert!(matches!(set, DecodeError::DuplicateItem { index: 1, .. }));

        let map = Map::decode(&[2, 0x01, 0x00, 0xAA, 0x01, 0x00, 0xBB], &((), ())).unwrap_err();
        assert!(matches!(map, DecodeError::DuplicateItem { index: 1, .. }));
    }

    /// The same entries as an accepted encoding, in the other order: accepting
    /// both would give the collection two encodings.
    #[test]
    fn decode_rejects_keys_out_of_order() {
        let set = Set::decode(&[2, 0x00, 0x01, 0x01, 0x00], &()).unwrap_err();
        assert!(matches!(set, DecodeError::OutOfOrderItem { index: 1, .. }));

        let map = BTreeMap::<u8, u8>::decode(&[2, 0, 0, 0, 2, 20, 1, 10], &((), ())).unwrap_err();
        assert!(matches!(map, DecodeError::OutOfOrderItem { index: 1, .. }));
    }

    /// The key alone decides that an entry is misplaced, so its value is never
    /// read: here it is not even there.
    #[test]
    fn decode_rejects_a_misplaced_key_before_decoding_its_value() {
        let repeated = Map::decode(&[2, 0x01, 0x00, 0xAA, 0x01, 0x00], &((), ())).unwrap_err();
        assert!(matches!(
            repeated,
            DecodeError::DuplicateItem { index: 1, .. }
        ));

        let unordered = Map::decode(&[2, 0x02, 0x00, 0xAA, 0x01, 0x00], &((), ())).unwrap_err();
        assert!(matches!(
            unordered,
            DecodeError::OutOfOrderItem { index: 1, .. }
        ));
    }

    #[test]
    fn decode_rejects_a_length_outside_the_bounds_before_decoding_entries() {
        let too_few = Set::decode(&[1, 0x01, 0x00], &()).unwrap_err();
        assert!(matches!(
            too_few,
            DecodeError::LengthOutOfBounds { len: 1, .. }
        ));

        let too_many = Map::decode(&[5], &((), ())).unwrap_err();
        assert!(matches!(
            too_many,
            DecodeError::LengthOutOfBounds { len: 5, .. }
        ));
    }

    #[test]
    fn decode_leaves_trailing_bytes_untouched() {
        let (rest, set) = BTreeSet::<u8>::decode(&[1, 0, 0, 0, 7, 0xCC], &()).unwrap();

        assert_eq!(rest, &[0xCC]);
        assert_eq!(set, BTreeSet::from(7));
    }

    /// Every key of a zero-length type decodes to the same value, so the
    /// second entry repeats the first. A four-byte prefix cannot buy
    /// `u32::MAX` iterations.
    #[test]
    fn a_zero_length_key_type_cannot_drive_an_unbounded_loop() {
        let set = BTreeSet::<[u8; 0]>::decode(&u32::MAX.to_le_bytes(), &()).unwrap_err();
        assert!(matches!(set, DecodeError::DuplicateItem { index: 1, .. }));

        let mut input = u32::MAX.to_le_bytes().to_vec();
        input.extend_from_slice(&[0xAA, 0xBB]);
        let map = BTreeMap::<[u8; 0], u8>::decode(&input, &((), ())).unwrap_err();
        assert!(matches!(map, DecodeError::DuplicateItem { index: 1, .. }));
    }

    #[test]
    fn a_large_declared_length_does_not_preallocate_from_the_wire() {
        // Declares `u32::MAX` entries but carries only one.
        let mut input = u32::MAX.to_le_bytes().to_vec();
        input.extend_from_slice(&7u64.to_le_bytes());
        input.extend_from_slice(&8u64.to_le_bytes());

        let (err, allocated) =
            bytes_allocated_by(|| BTreeMap::<u64, u64>::decode(&input, &((), ())).unwrap_err());

        assert!(matches!(err, DecodeError::UnexpectedEnd { .. }));
        assert!(
            allocated < 4096,
            "decoding a {} byte input allocated {allocated} bytes",
            input.len(),
        );
    }

    #[test]
    fn fixtures_hold() {
        assert_codec_fixtures::<BTreeSet<u8>>();
        assert_codec_fixtures_with::<BTreeMap<u8, u8>, _>(|| ((), ()));
        assert_codec_fixtures::<Set>();
        assert_codec_fixtures_with::<Map, _>(|| ((), ()));
    }

    #[test]
    fn fixtures_are_the_empty_collection_and_the_distinct_keys_in_order() {
        // `u8` declares its fixtures as `07` then `00`. Sorted, key `00` comes
        // first, and the values cycle through the fixtures in that order.
        let fixtures = BTreeMap::<u8, u8>::fixtures();
        let bytes: Vec<&[u8]> = fixtures
            .iter()
            .map(|fixture| fixture.bytes.as_ref())
            .collect();

        assert_eq!(
            bytes,
            [&[0, 0, 0, 0][..], &[2, 0, 0, 0, 0x00, 0x07, 0x07, 0x00]]
        );
    }

    #[test]
    fn fixtures_respect_small_bounds() {
        assert_codec_fixtures::<BoundedBTreeSet<u8, 0, 0>>();
        assert_codec_fixtures::<BoundedBTreeSet<u8, 1, 1>>();
        assert_codec_fixtures_with::<BoundedBTreeMap<u16, bool, 2, 2>, _>(|| ((), ()));
    }

    #[test]
    #[should_panic(expected = "needs at least 3 distinct fixtures of u8")]
    fn fixtures_panic_when_the_key_has_too_few_distinct_fixtures() {
        drop(BoundedBTreeSet::<u8, 3, 4>::fixtures());
    }
}
