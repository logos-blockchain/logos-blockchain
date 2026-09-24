//! Canonical codec for [`BoundedOrderedSet`]: a `MAX`-width length prefix,
//! then the elements in the set's own order, exactly as a bounded vector.
//!
//! The order is part of the value, so decoding keeps it. What the codec adds
//! to the vector's is that a repeated element is rejected where it appears.
//! That also bounds an element type that decodes without consuming input: its
//! second element is a duplicate, so no length prefix can make the decode loop
//! spin.

use core::{
    any::type_name,
    hash::{BuildHasher, Hash},
};
use std::borrow::Cow;

use indexmap::IndexSet;
use lb_utils::bounded::BoundedOrderedSet;

use super::{
    BinaryDecode, BinaryEncode, CodecExamples, CodecFixture, CodecFixtures, DecodeError,
    fixtures::distinct_fixtures_in_declared_order,
    length_prefix::{decode_bounded_length, encode_length_prefix_into, length_prefix_len},
    sealed,
};

impl<T, S, const MIN: usize, const MAX: usize> BinaryEncode for BoundedOrderedSet<T, MIN, MAX, S>
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
        for element in self {
            element.encode_into(out);
        }
    }
}

impl<T, S, const MIN: usize, const MAX: usize> BinaryDecode for BoundedOrderedSet<T, MIN, MAX, S>
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

        // Not pre-allocated from `len`: the prefix is only a claim until the
        // elements behind it have actually been decoded.
        let mut set = IndexSet::with_hasher(S::default());
        for index in 0..len {
            let (next, element) = T::decode(rest, context)?;
            if !set.insert(element) {
                return Err(DecodeError::duplicate_item::<Self>(index));
            }
            rest = next;
        }
        Ok((rest, Self::new_unchecked(set.into())))
    }
}

impl<T, S, const MIN: usize, const MAX: usize> sealed::Sealed for BoundedOrderedSet<T, MIN, MAX, S> where
    T: CodecExamples
{
}

// Derived from the element's fixtures, like the `BoundedVec` blanket, so every
// monomorphization has a fixture at compile time. A set cannot repeat an
// element, so it takes as many *distinct* element fixtures as `MAX` allows, in
// the order the element declares them. With fewer than `MIN` of them, building
// the fixture panics with instructions.
impl<T, S, const MIN: usize, const MAX: usize> CodecExamples for BoundedOrderedSet<T, MIN, MAX, S>
where
    T: CodecExamples + Eq + Hash,
    S: BuildHasher + Default,
{
    fn fixtures() -> CodecFixtures<Self> {
        let elements = distinct_fixtures_in_declared_order::<Self, T, MIN, MAX>();

        let mut bytes = Vec::new();
        encode_length_prefix_into::<MAX>(elements.len(), &mut bytes);
        let mut set = IndexSet::with_capacity_and_hasher(elements.len(), S::default());
        for element in elements {
            bytes.extend_from_slice(element.bytes.as_ref());
            assert!(
                set.insert(element.value),
                "two fixtures of {} with different bytes compare equal",
                type_name::<T>(),
            );
        }

        [CodecFixture {
            value: Self::new_unchecked(set.into()),
            bytes: Cow::Owned(bytes),
        }]
        .into()
    }
}

#[cfg(test)]
mod tests {
    use lb_utils::bounded::BoundedOrderedSet;

    use crate::canonical::{
        BinaryDecodeExt as _, BinaryEncode as _, CodecExamples as _, DecodeError,
        assert_codec_fixtures, tests::allocation::bytes_allocated_by,
    };

    /// Bound used across the tests: between 2 and 4 elements.
    type Set = BoundedOrderedSet<u16, 2, 4>;

    fn set(elements: &[u16]) -> Set {
        Set::try_from_iter(elements.iter().copied()).unwrap()
    }

    fn elements(set: &Set) -> Vec<u16> {
        set.iter().copied().collect()
    }

    #[test]
    fn encode_keeps_insertion_order() {
        // Count `02`, then `0100`, then `0001`: the vector's format.
        assert_eq!(hex::encode(set(&[1, 256]).encode()), "0201000001");
    }

    #[test]
    fn encoding_agrees_with_equality() {
        let forward = set(&[1, 2]);
        let same = set(&[1, 2]);
        let backward = set(&[2, 1]);

        assert_eq!(forward, same);
        assert_eq!(forward.encode(), same.encode());
        assert_ne!(forward, backward);
        assert_ne!(forward.encode(), backward.encode());
    }

    #[test]
    fn decode_keeps_the_encoded_order() {
        let bytes = [2, 0x01, 0x00, 0x00, 0x01];

        let (rest, decoded) = Set::decode(&bytes).unwrap();

        assert!(rest.is_empty());
        assert_eq!(elements(&decoded), [1, 256]);
        assert_eq!(decoded.encode_to_vec(), bytes);
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
        let (rest, decoded) = Set::decode(&[2, 0x01, 0x00, 0x00, 0x01, 0xCC]).unwrap();

        assert_eq!(rest, &[0xCC]);
        assert_eq!(elements(&decoded), [1, 256]);
    }

    /// Every element of a zero-length type decodes to the same value, so the
    /// second one is a duplicate. An 8-byte prefix cannot buy `u64::MAX`
    /// iterations.
    #[test]
    fn a_zero_length_element_type_cannot_drive_an_unbounded_loop() {
        type ZeroLength = BoundedOrderedSet<[u8; 0], 0, { u64::MAX as usize }>;

        let err = ZeroLength::decode(&[0xFF; 8]).unwrap_err();

        assert!(matches!(err, DecodeError::DuplicateItem { index: 1, .. }));
    }

    #[test]
    fn a_large_declared_length_does_not_preallocate_from_the_wire() {
        type Wide = BoundedOrderedSet<u64, 1, { u16::MAX as usize }>;

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
    fn fixtures_keep_the_declared_order_of_the_element_fixtures() {
        // Elements `07` then `00`, as `u8` declares them.
        let fixtures = BoundedOrderedSet::<u8, 0, 4>::fixtures();
        let fixture = fixtures.first().unwrap();

        assert_eq!(fixture.bytes.as_ref(), &[2, 0x07, 0x00]);
        assert_codec_fixtures::<BoundedOrderedSet<u8, 0, 4>>();
    }

    #[test]
    fn fixtures_respect_small_bounds() {
        assert_codec_fixtures::<BoundedOrderedSet<u8, 0, 0>>();
        assert_codec_fixtures::<BoundedOrderedSet<u8, 1, 1>>();
        assert_codec_fixtures::<BoundedOrderedSet<u16, 2, 2>>();
    }

    #[test]
    #[should_panic(expected = "needs at least 3 distinct fixtures of u8")]
    fn fixtures_panic_when_the_element_has_too_few_distinct_fixtures() {
        drop(BoundedOrderedSet::<u8, 3, 4>::fixtures());
    }
}
