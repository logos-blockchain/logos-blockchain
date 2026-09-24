//! Canonical codec for [`BoundedSet`]: a `MAX`-width length prefix, then the
//! elements in ascending byte-wise order of their encodings (see
//! [`super::keyed`]).

use core::{
    any::type_name,
    hash::{BuildHasher, Hash},
};
use std::{borrow::Cow, collections::HashSet};

use lb_utils::bounded::BoundedSet;

use super::{
    BinaryDecode, BinaryEncode, CodecExamples, CodecFixture, CodecFixtures, DecodeError,
    keyed::{
        check_canonical_order, consumed, distinct_fixtures_in_canonical_order,
        encode_items_sorted_by_key,
    },
    length_prefix::{decode_bounded_length, encode_length_prefix_into, length_prefix_len},
    sealed,
};

impl<T, S, const MIN: usize, const MAX: usize> BinaryEncode for BoundedSet<T, MIN, MAX, S>
where
    T: BinaryEncode + Eq + Hash,
    S: BuildHasher + Default,
{
    fn encoded_length(&self) -> usize {
        length_prefix_len::<MAX>()
            .checked_add(self.iter().map(BinaryEncode::encoded_length).sum::<usize>())
            .expect("Encoded length overflow")
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        encode_length_prefix_into::<MAX>(self.len(), out);
        // To ensure encoding is independent of iteration order, we sort items by
        // encoded key.
        encode_items_sorted_by_key(self.iter(), out, |element, buffer| {
            let start = buffer.len();
            element.encode_into(buffer);
            buffer.len() - start
        });
    }
}

impl<T, S, const MIN: usize, const MAX: usize> BinaryDecode for BoundedSet<T, MIN, MAX, S>
where
    T: BinaryDecode + Eq + Hash,
    S: BuildHasher + Default,
{
    type Context = T::Context;

    fn decode<'input>(
        input: &'input [u8],
        context: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (mut rest, len) = decode_bounded_length::<Self, MIN, MAX>(input)?;

        let mut set = HashSet::with_hasher(S::default());
        let mut previous = None;
        for index in 0..len {
            let (next, element) = T::decode(rest, context)?;
            let encoded = consumed(rest, next);
            check_canonical_order::<Self>(previous, encoded, index)?;
            // Distinct bytes almost always mean distinct elements, but `Eq` is
            // the set's own notion of a duplicate.
            if !set.insert(element) {
                return Err(DecodeError::duplicate_item::<Self>(index));
            }
            previous = Some(encoded);
            rest = next;
        }
        Ok((rest, Self::new_unchecked(set)))
    }
}

impl<T, S, const MIN: usize, const MAX: usize> sealed::Sealed for BoundedSet<T, MIN, MAX, S> where
    T: CodecExamples
{
}

// Derived from the element's fixtures, like the `BoundedVec` blanket, so every
// monomorphization has a fixture at compile time. A set cannot repeat an
// element, so it takes as many *distinct* element fixtures as `MAX` allows,
// which also exercises the ordering whenever the element has two or more. With
// fewer than `MIN` of them, building the fixture panics with instructions.
impl<T, S, const MIN: usize, const MAX: usize> CodecExamples for BoundedSet<T, MIN, MAX, S>
where
    T: CodecExamples + Eq + Hash,
    S: BuildHasher + Default,
{
    fn fixtures() -> CodecFixtures<Self> {
        let elements = distinct_fixtures_in_canonical_order::<Self, T, MIN, MAX>();

        let mut bytes = Vec::new();
        encode_length_prefix_into::<MAX>(elements.len(), &mut bytes);
        let mut set = HashSet::with_capacity_and_hasher(elements.len(), S::default());
        for element in elements {
            bytes.extend_from_slice(element.bytes.as_ref());
            assert!(
                set.insert(element.value),
                "two fixtures of {} with different bytes compare equal",
                type_name::<T>(),
            );
        }

        [CodecFixture {
            value: Self::new_unchecked(set),
            bytes: Cow::Owned(bytes),
        }]
        .into()
    }
}

#[cfg(test)]
mod tests {
    use lb_utils::bounded::BoundedSet;

    use crate::canonical::{
        BinaryDecodeExt as _, BinaryEncode as _, CodecExamples as _, DecodeError,
        assert_codec_fixtures, tests::allocation::bytes_allocated_by,
    };

    /// Bound used across the tests: between 2 and 4 elements.
    type Set = BoundedSet<u16, 2, 4>;

    fn set(elements: &[u16]) -> Set {
        Set::try_from_iter(elements.iter().copied()).unwrap()
    }

    #[test]
    fn encode_sorts_elements_by_their_encoded_bytes() {
        // Little-endian, 256 encodes as `0001` and 1 as `0100`, so byte order
        // puts 256 first even though it is the larger number: count `02`, then
        // `0001`, then `0100`.
        assert_eq!(hex::encode(set(&[1, 256]).encode()), "0200010100");
    }

    #[test]
    fn encoding_does_not_depend_on_insertion_order() {
        let forward = set(&[1, 2, 3, 4]).encode();
        let backward = set(&[4, 3, 2, 1]).encode();

        assert_eq!(forward, backward);
        assert_eq!(hex::encode(forward), "040100020003000400");
    }

    #[test]
    fn decode_reads_elements_in_canonical_order() {
        let (rest, decoded) = Set::decode(&[2, 0x00, 0x01, 0x01, 0x00]).unwrap();

        assert!(rest.is_empty());
        assert_eq!(decoded, set(&[1, 256]));
    }

    #[test]
    fn decode_rejects_elements_out_of_canonical_order() {
        let err = Set::decode(&[2, 0x01, 0x00, 0x00, 0x01]).unwrap_err();

        assert!(matches!(
            err,
            DecodeError::NonCanonicalOrder { index: 1, .. }
        ));
    }

    #[test]
    fn decode_rejects_a_repeated_element() {
        let err = Set::decode(&[2, 0x01, 0x00, 0x01, 0x00]).unwrap_err();

        assert!(matches!(err, DecodeError::DuplicateItem { index: 1, .. }));
    }

    #[test]
    fn decode_rejects_a_length_outside_the_bounds_before_decoding_elements() {
        let too_few = Set::decode(&[1, 0x01, 0x00]).unwrap_err();
        assert!(matches!(
            too_few,
            DecodeError::LengthOutOfBounds { len: 1, .. }
        ));

        let too_many = Set::decode(&[5]).unwrap_err();
        assert!(matches!(
            too_many,
            DecodeError::LengthOutOfBounds { len: 5, .. }
        ));
    }

    #[test]
    fn decode_fails_when_the_elements_are_truncated() {
        let err = Set::decode(&[2, 0x01, 0x00, 0x02]).unwrap_err();

        assert!(matches!(err, DecodeError::UnexpectedEnd { .. }));
    }

    #[test]
    fn decode_leaves_trailing_bytes_untouched() {
        let (rest, decoded) = Set::decode(&[2, 0x00, 0x01, 0x01, 0x00, 0xAA]).unwrap();

        assert_eq!(rest, &[0xAA]);
        assert_eq!(decoded, set(&[1, 256]));
    }

    /// Every element of a zero-length type encodes to the same empty slice, so
    /// the second one is a duplicate. That is what stops an 8-byte prefix from
    /// buying `u64::MAX` iterations, with no special case for such elements.
    #[test]
    fn a_zero_length_element_type_cannot_drive_an_unbounded_loop() {
        type ZeroLength = BoundedSet<[u8; 0], 0, { u64::MAX as usize }>;

        let err = ZeroLength::decode(&[0xFF; 8]).unwrap_err();

        assert!(matches!(err, DecodeError::DuplicateItem { index: 1, .. }));
    }

    #[test]
    fn a_single_zero_length_element_roundtrips() {
        type ZeroLength = BoundedSet<[u8; 0], 0, 4>;

        let original = ZeroLength::try_from_iter([[]]).unwrap();
        let bytes = original.encode_to_vec();
        let decoded = ZeroLength::decode_all(&bytes).unwrap();

        assert_eq!(bytes, [1]);
        assert_eq!(decoded, original);
    }

    #[test]
    fn a_large_declared_length_does_not_preallocate_from_the_wire() {
        type Wide = BoundedSet<u64, 1, { u16::MAX as usize }>;

        // Declares `u16::MAX` elements but carries only one.
        let mut input = u16::MAX.to_le_bytes().to_vec();
        input.extend_from_slice(&7u64.to_le_bytes());

        let (err, allocated) = bytes_allocated_by(|| Wide::decode(&input).unwrap_err());

        assert!(matches!(err, DecodeError::UnexpectedEnd { .. }));
        assert!(
            allocated < 4096,
            "decoding a {} byte input allocated {allocated} bytes",
            input.len(),
        );
    }

    #[test]
    fn fixtures_use_every_distinct_element_fixture_up_to_max() {
        // `u8` has two fixtures, `07` and `00`, which the set lists in
        // canonical order.
        let fixtures = BoundedSet::<u8, 0, 4>::fixtures();
        let fixture = fixtures.first().unwrap();

        assert_eq!(fixture.bytes.as_ref(), &[2, 0x00, 0x07]);
        assert_codec_fixtures::<BoundedSet<u8, 0, 4>>();
    }

    #[test]
    fn fixtures_respect_small_bounds() {
        assert_codec_fixtures::<BoundedSet<u8, 0, 0>>();
        assert_codec_fixtures::<BoundedSet<u8, 1, 1>>();
        assert_codec_fixtures::<BoundedSet<u8, 2, 2>>();
        assert_codec_fixtures::<BoundedSet<u16, 0, { u16::MAX as usize }>>();
    }

    #[test]
    #[should_panic(expected = "needs at least 3 distinct fixtures of u8")]
    fn fixtures_panic_when_the_element_has_too_few_distinct_fixtures() {
        drop(BoundedSet::<u8, 3, 4>::fixtures());
    }
}
