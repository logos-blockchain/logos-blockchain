//! A runtime-bounded vector encodes as a `BoundedVec` does: its length, then
//! the items. The length is written as a `Max`, the type of the vector's
//! maximum, so it takes that type's width whatever the maximum's value: the
//! maximum is a check, never part of the encoding, and a vector encodes the
//! same under every maximum. A `u8` maximum gives the one-byte length of a
//! `BoundedVec` whose `MAX` is at most 255.
//!
//! The decoder has no vector yet, so its context gives the maximum, ahead of
//! the items' own context, and the decoder holds the length to it before a
//! single item is decoded.

use std::borrow::Cow;

use lb_utils::bounded::RuntimeBoundedVec;

use super::{
    BinaryDecode, BinaryEncode, CodecExamples, CodecFixture, CodecFixtures, DecodeError,
    bounded_vec::ZERO_LENGTH_ELEMENT_MAX, sealed,
};

/// The length of a vector of `len` items, as its maximum's type.
///
/// # Panics
///
/// If `len` does not fit a `Max`. A vector's length is at most its maximum,
/// itself a `Max`, so it always fits. The only case where this can happen is
/// via a wrongly-checked value before calling `new_unchecked`.
fn cast_length_to<Max>(len: usize) -> Max
where
    Max: TryFrom<usize>,
{
    let Ok(max) = Max::try_from(len) else {
        panic!("A vector's length is at most its maximum, so it fits its type.");
    };
    max
}

impl<T, Max, const MIN: usize> BinaryEncode for RuntimeBoundedVec<T, Max, MIN>
where
    T: BinaryEncode,
    Max: BinaryEncode + TryFrom<usize>,
{
    fn encoded_length(&self) -> usize {
        cast_length_to::<Max>(self.len())
            .encoded_length()
            .checked_add(self.iter().map(BinaryEncode::encoded_length).sum::<usize>())
            .expect("Encoded length overflow")
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        cast_length_to::<Max>(self.len()).encode_into(out);
        for item in self.iter() {
            item.encode_into(out);
        }
    }
}

impl<T, Max, const MIN: usize> BinaryDecode for RuntimeBoundedVec<T, Max, MIN>
where
    T: BinaryDecode,
    Max: BinaryEncode + BinaryDecode<Context = ()> + TryFrom<usize> + Copy + Into<usize>,
{
    /// The maximum length the vector is held to, then the items' context.
    type Context = (Max, T::Context);

    fn decode<'input>(
        input: &'input [u8],
        (max_len, context): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let max = (*max_len).into();

        // The length is checked before decoding, so an oversized prefix never
        // causes us to decode a too-large payload.
        let (mut rest, len) = Max::decode(input, &())?;
        let len = len.into();
        if len < MIN || len > max {
            return Err(DecodeError::length_out_of_bounds::<Self>(len, MIN, max));
        }

        let mut items = Vec::new();

        for _ in 0..len {
            let (next, item) = T::decode(rest, context)?;

            // An item that decodes without consuming input makes the loop
            // independent of the input, so it is refused unless the maximum is
            // small enough, as under a static bound.
            if next.len() == rest.len() && max > ZERO_LENGTH_ELEMENT_MAX {
                return Err(DecodeError::zero_length_element::<T>(
                    max,
                    ZERO_LENGTH_ELEMENT_MAX,
                ));
            }

            rest = next;
            items.push(item);
        }
        Ok((rest, Self::new_unchecked(items, *max_len)))
    }
}

impl<T, Max, const MIN: usize> sealed::Sealed for RuntimeBoundedVec<T, Max, MIN> where
    T: CodecExamples
{
}

// The fixture is derived from the element's fixture, as a `BoundedVec`'s is,
// with at least one element so that it touches `T`'s codec. Its maximum is its
// length, the smallest that admits it: it round-trips under a context giving
// it.
impl<T, Max, const MIN: usize> CodecExamples for RuntimeBoundedVec<T, Max, MIN>
where
    T: CodecExamples,
    Max: BinaryEncode + TryFrom<usize>,
{
    fn fixtures() -> CodecFixtures<Self> {
        let count = MIN.max(1);
        let max_len = cast_length_to::<Max>(count);

        let mut values = Vec::with_capacity(count);
        let mut bytes = max_len.encode_to_vec();
        for _ in 0..count {
            let item = T::fixtures()
                .into_iter()
                .next()
                .expect("`CodecExamples::fixtures` is non-empty");
            bytes.extend_from_slice(item.bytes.as_ref());
            values.push(item.value);
        }

        [CodecFixture {
            value: Self::new_unchecked(values, max_len),
            bytes: Cow::Owned(bytes),
        }]
        .into()
    }
}

#[cfg(test)]
mod tests {
    use lb_utils::bounded::{BoundedVec, RuntimeBoundedVec, UpperRuntimeBoundedVec};

    use crate::canonical::{
        BinaryDecode as _, BinaryEncode as _, DecodeError, assert_codec_fixtures_with,
        bounded_vec::ZERO_LENGTH_ELEMENT_MAX,
    };

    /// Between 2 elements and a `u8` maximum given at runtime.
    type Min2 = RuntimeBoundedVec<u8, u8, 2>;

    /// The length takes the width of the maximum's type, whatever the
    /// maximum's value, so a vector encodes the same under every maximum.
    #[test]
    fn the_length_is_as_wide_as_the_maximum_type() {
        for max_len in [3, 200, u8::MAX] {
            let bounded = Min2::try_new(vec![1, 2, 3], max_len).unwrap();

            assert_eq!(bounded.encode_to_vec(), [3, 1, 2, 3]);
        }

        let under_a_u16 = RuntimeBoundedVec::<u8, u16, 2>::try_new(vec![1, 2, 3], 3).unwrap();

        assert_eq!(under_a_u16.encode_to_vec(), [3, 0, 1, 2, 3]);
    }

    #[test]
    fn it_encodes_as_a_bounded_vec_whose_maximum_fits_the_same_type() {
        let runtime = UpperRuntimeBoundedVec::<u16, u8>::try_new(vec![1, 2, 3], 4).unwrap();
        let fixed = BoundedVec::<u16, 0, 4>::try_from(vec![1, 2, 3]).unwrap();

        assert_eq!(runtime.encode_to_vec(), fixed.encode_to_vec());
    }

    #[test]
    fn decoding_holds_the_length_to_the_maximum_in_the_context() {
        let bytes = [3, 1, 2, 3];

        let (rest, decoded) = Min2::decode(&bytes, &(3, ())).unwrap();
        let err = Min2::decode(&bytes, &(2, ())).unwrap_err();

        assert_eq!(rest, b"");
        assert_eq!(decoded.as_slice(), &[1, 2, 3]);
        assert_eq!(decoded.max_len(), 3);
        assert!(matches!(
            err,
            DecodeError::LengthOutOfBounds {
                len: 3,
                min: 2,
                max: 2,
                ..
            }
        ));
    }

    /// A length past the maximum fails as such, not on the items missing
    /// behind it: the length is checked before any item is decoded.
    #[test]
    fn a_length_past_the_maximum_is_refused_before_the_items() {
        let err = Min2::decode(&[5], &(4, ())).unwrap_err();

        assert!(matches!(err, DecodeError::LengthOutOfBounds { len: 5, .. }));
    }

    #[test]
    fn a_length_below_the_minimum_is_refused() {
        let err = Min2::decode(&[1, 7], &(4, ())).unwrap_err();

        assert!(matches!(err, DecodeError::LengthOutOfBounds { len: 1, .. }));
    }

    /// Whether an item type that decodes without consuming input is supported
    /// turns on the maximum in the context, as it turns on `MAX` under a
    /// static bound.
    #[test]
    fn zero_length_item_support_turns_on_the_maximum() {
        type ZeroLength = UpperRuntimeBoundedVec<[u8; 0], u16>;
        let supported = u16::try_from(ZERO_LENGTH_ELEMENT_MAX).unwrap();
        // A two-byte length, declaring one item.
        let input = [1, 0];

        let (_, decoded) = ZeroLength::decode(&input, &(supported, ())).unwrap();
        let err = ZeroLength::decode(&input, &(supported + 1, ())).unwrap_err();

        assert_eq!(decoded.len(), 1);
        assert!(matches!(err, DecodeError::ZeroLengthElement { .. }));
    }

    #[test]
    fn fixtures_roundtrip_under_their_maximum() {
        assert_codec_fixtures_with::<Min2, _>(|| (2, ()));
    }
}
