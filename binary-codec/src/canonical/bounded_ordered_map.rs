//! Canonical codec for [`BoundedOrderedMap`]: a `MAX`-width length prefix,
//! then each entry as its key followed by its value, in the map's own order,
//! exactly as a bounded vector of pairs.
//!
//! The order is part of the value, so decoding keeps it. What the codec adds
//! to the vector's is that a repeated key is rejected where it appears, before
//! its value is decoded. That also bounds a key type that decodes without
//! consuming input: its second key is a duplicate, so no length prefix can
//! make the decode loop spin.

use core::{
    any::type_name,
    hash::{BuildHasher, Hash},
};
use std::borrow::Cow;

use indexmap::IndexMap;
use lb_utils::bounded::BoundedOrderedMap;

use super::{
    BinaryDecode, BinaryEncode, CodecExamples, CodecFixture, CodecFixtures, DecodeError,
    fixtures::{cycled_fixture, distinct_fixtures_in_declared_order},
    length_prefix::{decode_bounded_length, encode_length_prefix_into, length_prefix_len},
    sealed,
};

impl<K, V, S, const MIN: usize, const MAX: usize> BinaryEncode
    for BoundedOrderedMap<K, V, MIN, MAX, S>
where
    K: BinaryEncode + Eq + Hash,
    V: BinaryEncode,
    S: BuildHasher + Default,
{
    fn encoded_length(&self) -> usize {
        length_prefix_len::<MAX>()
            .checked_add(
                self.iter()
                    .map(|(key, value)| key.encoded_length() + value.encoded_length())
                    .sum::<usize>(),
            )
            .expect("Encoded length overflow")
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        encode_length_prefix_into::<MAX>(self.len(), out);
        for (key, value) in self {
            key.encode_into(out);
            value.encode_into(out);
        }
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> BinaryDecode
    for BoundedOrderedMap<K, V, MIN, MAX, S>
where
    K: BinaryDecode + Eq + Hash,
    V: BinaryDecode,
    S: BuildHasher + Default,
{
    type Context = (K::Context, V::Context);

    fn decode<'input>(
        input: &'input [u8],
        context: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (mut rest, len) = decode_bounded_length::<Self, MIN, MAX>(input)?;

        // Not pre-allocated from `len`: the prefix is only a claim until the
        // entries behind it have actually been decoded.
        let mut map = IndexMap::with_hasher(S::default());
        for index in 0..len {
            let (after_key, key) = K::decode(rest, &context.0)?;
            // Checked before the value is decoded, so a repeated key costs
            // nothing more.
            if map.contains_key(&key) {
                return Err(DecodeError::duplicate_item::<Self>(index));
            }
            let (next, value) = V::decode(after_key, &context.1)?;
            map.insert(key, value);
            rest = next;
        }
        Ok((rest, Self::new_unchecked(map.into())))
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> sealed::Sealed
    for BoundedOrderedMap<K, V, MIN, MAX, S>
where
    K: CodecExamples,
    V: CodecExamples,
{
}

// Keys come from the key type's distinct fixtures, in the order it declares
// them, as elements do for `BoundedOrderedSet`; values cycle through the value
// type's fixtures, so any value type works.
impl<K, V, S, const MIN: usize, const MAX: usize> CodecExamples
    for BoundedOrderedMap<K, V, MIN, MAX, S>
where
    K: CodecExamples + Eq + Hash,
    V: CodecExamples,
    S: BuildHasher + Default,
{
    fn fixtures() -> CodecFixtures<Self> {
        let keys = distinct_fixtures_in_declared_order::<Self, K, MIN, MAX>();

        let mut bytes = Vec::new();
        encode_length_prefix_into::<MAX>(keys.len(), &mut bytes);
        let mut map = IndexMap::with_capacity_and_hasher(keys.len(), S::default());
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

        [CodecFixture {
            value: Self::new_unchecked(map.into()),
            bytes: Cow::Owned(bytes),
        }]
        .into()
    }
}

#[cfg(test)]
mod tests {
    use lb_utils::bounded::BoundedOrderedMap;

    use crate::canonical::{
        BinaryDecode as _, BinaryEncode as _, CodecExamples as _, DecodeError,
        assert_codec_fixtures_with, tests::allocation::bytes_allocated_by,
    };

    /// Bound used across the tests: between 2 and 4 entries.
    type Map = BoundedOrderedMap<u16, u8, 2, 4>;

    fn map(entries: &[(u16, u8)]) -> Map {
        Map::try_from_iter(entries.iter().copied()).unwrap()
    }

    fn entries(map: &Map) -> Vec<(u16, u8)> {
        map.iter().map(|(key, value)| (*key, *value)).collect()
    }

    #[test]
    fn encode_keeps_insertion_order() {
        // Count `02`, then `0100` `aa`, then `0001` `bb`: the vector's format.
        assert_eq!(
            hex::encode(map(&[(1, 0xAA), (256, 0xBB)]).encode()),
            "020100aa0001bb"
        );
    }

    #[test]
    fn encoding_agrees_with_equality() {
        let forward = map(&[(1, 10), (2, 20)]);
        let same = map(&[(1, 10), (2, 20)]);
        let backward = map(&[(2, 20), (1, 10)]);

        assert_eq!(forward, same);
        assert_eq!(forward.encode(), same.encode());
        assert_ne!(forward, backward);
        assert_ne!(forward.encode(), backward.encode());
    }

    #[test]
    fn decode_keeps_the_encoded_order() {
        let bytes = [2, 0x01, 0x00, 0xAA, 0x00, 0x01, 0xBB];

        let (rest, decoded) = Map::decode(&bytes, &((), ())).unwrap();

        assert!(rest.is_empty());
        assert_eq!(entries(&decoded), [(1, 0xAA), (256, 0xBB)]);
        assert_eq!(decoded.encode_to_vec(), bytes);
    }

    #[test]
    fn decode_rejects_a_repeated_key_even_with_a_different_value() {
        let err = Map::decode(&[2, 0x01, 0x00, 0xAA, 0x01, 0x00, 0xBB], &((), ())).unwrap_err();

        assert!(matches!(err, DecodeError::DuplicateItem { index: 1, .. }));
    }

    #[test]
    fn decode_rejects_a_length_outside_the_bounds_before_decoding_entries() {
        let too_few = Map::decode(&[1, 0x01, 0x00, 0xAA], &((), ())).unwrap_err();
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
    fn decode_fails_when_a_value_is_truncated() {
        let err = Map::decode(&[2, 0x00, 0x01, 0xBB, 0x01, 0x00], &((), ())).unwrap_err();

        assert!(matches!(err, DecodeError::UnexpectedEnd { .. }));
    }

    #[test]
    fn decode_leaves_trailing_bytes_untouched() {
        let (rest, decoded) =
            Map::decode(&[2, 0x01, 0x00, 0xAA, 0x00, 0x01, 0xBB, 0xCC], &((), ())).unwrap();

        assert_eq!(rest, &[0xCC]);
        assert_eq!(entries(&decoded), [(1, 0xAA), (256, 0xBB)]);
    }

    /// Every key of a zero-length type decodes to the same value, so the
    /// second entry repeats the first. An 8-byte prefix cannot buy `u64::MAX`
    /// iterations.
    #[test]
    fn a_zero_length_key_type_cannot_drive_an_unbounded_loop() {
        type ZeroLength = BoundedOrderedMap<[u8; 0], u8, 0, { u64::MAX as usize }>;

        let mut input = u64::MAX.to_le_bytes().to_vec();
        input.extend_from_slice(&[0xAA, 0xBB]);

        let err = ZeroLength::decode(&input, &((), ())).unwrap_err();

        assert!(matches!(err, DecodeError::DuplicateItem { index: 1, .. }));
    }

    #[test]
    fn a_large_declared_length_does_not_preallocate_from_the_wire() {
        type Wide = BoundedOrderedMap<u64, u64, 1, { u16::MAX as usize }>;

        // Declares `u16::MAX` entries but carries only one.
        let mut input = u16::MAX.to_le_bytes().to_vec();
        input.extend_from_slice(&7u64.to_le_bytes());
        input.extend_from_slice(&8u64.to_le_bytes());

        let (err, allocated) = bytes_allocated_by(|| Wide::decode(&input, &((), ())).unwrap_err());

        assert!(matches!(err, DecodeError::UnexpectedEnd { .. }));
        assert!(
            allocated < 4096,
            "decoding a {} byte input allocated {allocated} bytes",
            input.len(),
        );
    }

    #[test]
    fn fixtures_keep_the_declared_order_of_the_key_fixtures() {
        // Keys `07` then `00`, as `u8` declares them; values cycle `07` then
        // `00`.
        let fixtures = BoundedOrderedMap::<u8, u8, 0, 4>::fixtures();
        let fixture = fixtures.first().unwrap();

        assert_eq!(fixture.bytes.as_ref(), &[2, 0x07, 0x07, 0x00, 0x00]);
        assert_codec_fixtures_with::<BoundedOrderedMap<u8, u8, 0, 4>, _>(|| ((), ()));
    }

    #[test]
    fn fixtures_respect_small_bounds() {
        assert_codec_fixtures_with::<BoundedOrderedMap<u8, u16, 0, 0>, _>(|| ((), ()));
        assert_codec_fixtures_with::<BoundedOrderedMap<u8, u16, 1, 1>, _>(|| ((), ()));
        assert_codec_fixtures_with::<BoundedOrderedMap<u16, bool, 2, 2>, _>(|| ((), ()));
    }

    #[test]
    #[should_panic(expected = "needs at least 3 distinct fixtures of u8")]
    fn fixtures_panic_when_the_key_has_too_few_distinct_fixtures() {
        drop(BoundedOrderedMap::<u8, u8, 3, 4>::fixtures());
    }
}
