//! Canonical codec for [`BoundedMap`]: a `MAX`-width length prefix, then each
//! entry as its key followed by its value, in ascending byte-wise order of the
//! encoded keys (see [`super::keyed`]).

use core::{
    any::type_name,
    hash::{BuildHasher, Hash},
};
use std::{borrow::Cow, collections::HashMap};

use lb_utils::bounded::BoundedMap;

use super::{
    BinaryDecode, BinaryEncode, CodecExamples, CodecFixture, CodecFixtures, DecodeError,
    keyed::{
        check_canonical_order, consumed, cycled_fixture, distinct_fixtures_in_canonical_order,
        encode_items_sorted_by_key,
    },
    length_prefix::{decode_bounded_length, encode_length_prefix_into, length_prefix_len},
    sealed,
};

impl<K, V, S, const MIN: usize, const MAX: usize> BinaryEncode for BoundedMap<K, V, MIN, MAX, S>
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
        encode_items_sorted_by_key(self.iter(), out, |(key, value), buffer| {
            let start = buffer.len();
            key.encode_into(buffer);
            let key_len = buffer.len() - start;
            value.encode_into(buffer);
            key_len
        });
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> BinaryDecode for BoundedMap<K, V, MIN, MAX, S>
where
    K: BinaryDecode<Context = ()> + Eq + Hash,
    V: BinaryDecode,
    S: BuildHasher + Default,
{
    type Context = V::Context;

    fn decode<'input>(
        input: &'input [u8],
        context: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (mut rest, len) = decode_bounded_length::<Self, MIN, MAX>(input)?;

        let mut map = HashMap::with_hasher(S::default());
        let mut previous_key = None;
        for index in 0..len {
            let (after_key, key) = K::decode(rest, &())?;
            let encoded_key = consumed(rest, after_key);
            // Checked before the value is decoded, so a bad key costs nothing
            // more.
            check_canonical_order::<Self>(previous_key, encoded_key, index)?;
            if map.contains_key(&key) {
                return Err(DecodeError::duplicate_item::<Self>(index));
            }
            let (next, value) = V::decode(after_key, context)?;
            map.insert(key, value);
            previous_key = Some(encoded_key);
            rest = next;
        }
        Ok((rest, Self::new_unchecked(map)))
    }
}

impl<K, V, S, const MIN: usize, const MAX: usize> sealed::Sealed for BoundedMap<K, V, MIN, MAX, S>
where
    K: CodecExamples,
    V: CodecExamples,
{
}

// Keys come from the key type's distinct fixtures, as for `BoundedSet`; values
// cycle through the value type's fixtures, so any value type works.
impl<K, V, S, const MIN: usize, const MAX: usize> CodecExamples for BoundedMap<K, V, MIN, MAX, S>
where
    K: CodecExamples + Eq + Hash,
    V: CodecExamples,
    S: BuildHasher + Default,
{
    fn fixtures() -> CodecFixtures<Self> {
        let keys = distinct_fixtures_in_canonical_order::<Self, K, MIN, MAX>();

        let mut bytes = Vec::new();
        encode_length_prefix_into::<MAX>(keys.len(), &mut bytes);
        let mut map = HashMap::with_capacity_and_hasher(keys.len(), S::default());
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
            value: Self::new_unchecked(map),
            bytes: Cow::Owned(bytes),
        }]
        .into()
    }
}

#[cfg(test)]
mod tests {
    use lb_utils::bounded::BoundedMap;

    use crate::canonical::{
        BinaryDecodeExt as _, BinaryEncode as _, CodecExamples as _, DecodeError,
        assert_codec_fixtures, tests::allocation::bytes_allocated_by,
    };

    /// Bound used across the tests: between 2 and 4 entries.
    type Map = BoundedMap<u16, u8, 2, 4>;

    fn map(entries: &[(u16, u8)]) -> Map {
        Map::try_from_iter(entries.iter().copied()).unwrap()
    }

    #[test]
    fn encode_sorts_entries_by_their_encoded_keys() {
        // Key 256 encodes as `0001` and key 1 as `0100`, so the entry for 256
        // comes first: count `02`, then `0001` `bb`, then `0100` `aa`.
        assert_eq!(
            hex::encode(map(&[(1, 0xAA), (256, 0xBB)]).encode()),
            "020001bb0100aa"
        );
    }

    #[test]
    fn encoding_does_not_depend_on_insertion_order() {
        let forward = map(&[(1, 10), (2, 20), (3, 30)]).encode();
        let backward = map(&[(3, 30), (2, 20), (1, 10)]).encode();

        assert_eq!(forward, backward);
    }

    #[test]
    fn decode_reads_entries_in_canonical_order() {
        let (rest, decoded) = Map::decode(&[2, 0x00, 0x01, 0xBB, 0x01, 0x00, 0xAA]).unwrap();

        assert!(rest.is_empty());
        assert_eq!(decoded, map(&[(1, 0xAA), (256, 0xBB)]));
    }

    #[test]
    fn decode_rejects_keys_out_of_canonical_order() {
        let err = Map::decode(&[2, 0x01, 0x00, 0xAA, 0x00, 0x01, 0xBB]).unwrap_err();

        assert!(matches!(
            err,
            DecodeError::NonCanonicalOrder { index: 1, .. }
        ));
    }

    #[test]
    fn decode_rejects_a_repeated_key_even_with_a_different_value() {
        let err = Map::decode(&[2, 0x01, 0x00, 0xAA, 0x01, 0x00, 0xBB]).unwrap_err();

        assert!(matches!(err, DecodeError::DuplicateItem { index: 1, .. }));
    }

    #[test]
    fn decode_rejects_a_length_outside_the_bounds_before_decoding_entries() {
        let too_few = Map::decode(&[1, 0x01, 0x00, 0xAA]).unwrap_err();
        assert!(matches!(
            too_few,
            DecodeError::LengthOutOfBounds { len: 1, .. }
        ));

        let too_many = Map::decode(&[5]).unwrap_err();
        assert!(matches!(
            too_many,
            DecodeError::LengthOutOfBounds { len: 5, .. }
        ));
    }

    #[test]
    fn decode_fails_when_a_value_is_truncated() {
        let err = Map::decode(&[2, 0x00, 0x01, 0xBB, 0x01, 0x00]).unwrap_err();

        assert!(matches!(err, DecodeError::UnexpectedEnd { .. }));
    }

    #[test]
    fn a_large_declared_length_does_not_preallocate_from_the_wire() {
        type Wide = BoundedMap<u64, u64, 1, { u16::MAX as usize }>;

        // Declares `u16::MAX` entries but carries only one.
        let mut input = u16::MAX.to_le_bytes().to_vec();
        input.extend_from_slice(&7u64.to_le_bytes());
        input.extend_from_slice(&8u64.to_le_bytes());

        let (err, allocated) = bytes_allocated_by(|| Wide::decode(&input).unwrap_err());

        assert!(matches!(err, DecodeError::UnexpectedEnd { .. }));
        assert!(
            allocated < 4096,
            "decoding a {} byte input allocated {allocated} bytes",
            input.len(),
        );
    }

    #[test]
    fn fixtures_pair_distinct_keys_with_cycled_values() {
        // Keys `00` and `07` in canonical order; values `07` then `00`, the
        // `u8` fixtures in declaration order.
        let fixtures = BoundedMap::<u8, u8, 0, 4>::fixtures();
        let fixture = fixtures.first().unwrap();

        assert_eq!(fixture.bytes.as_ref(), &[2, 0x00, 0x07, 0x07, 0x00]);
        assert_codec_fixtures::<BoundedMap<u8, u8, 0, 4>>();
    }

    #[test]
    fn fixtures_respect_small_bounds() {
        assert_codec_fixtures::<BoundedMap<u8, u16, 0, 0>>();
        assert_codec_fixtures::<BoundedMap<u8, u16, 1, 1>>();
        assert_codec_fixtures::<BoundedMap<u16, bool, 2, 2>>();
    }

    #[test]
    #[should_panic(expected = "needs at least 3 distinct fixtures of u8")]
    fn fixtures_panic_when_the_key_has_too_few_distinct_fixtures() {
        drop(BoundedMap::<u8, u8, 3, 4>::fixtures());
    }
}
