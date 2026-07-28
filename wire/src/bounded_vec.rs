use std::borrow::Cow;

use lb_utils::bounded::BoundedVec;

use crate::{sealed, DecodeError, WireDecode, WireEncode, WireExamples, WireFixture, WireFixtures};

#[derive(Debug, Clone, Copy)]
enum NOfBytes {
    One,
    Two,
    Four,
    Eight,
}

const fn length_prefix_width<const MAX_LENGTH: usize>() -> NOfBytes {
    if MAX_LENGTH <= u8::MAX as usize {
        NOfBytes::One
    } else if MAX_LENGTH <= u16::MAX as usize {
        NOfBytes::Two
    } else if MAX_LENGTH <= u32::MAX as usize {
        NOfBytes::Four
    } else {
        NOfBytes::Eight
    }
}

/// Byte-width of the length prefix for a `MAX_LENGTH`-bounded collection.
const fn length_prefix_len<const MAX_LENGTH: usize>() -> usize {
    match length_prefix_width::<MAX_LENGTH>() {
        NOfBytes::One => 1,
        NOfBytes::Two => 2,
        NOfBytes::Four => 4,
        NOfBytes::Eight => 8,
    }
}

fn encode_length_prefix_into<const MAX_LENGTH: usize>(actual_length: usize, out: &mut Vec<u8>) {
    match length_prefix_width::<MAX_LENGTH>() {
        NOfBytes::One => u8::try_from(actual_length)
            .expect("Actual length should be smaller than u8 MAX_LENGTH")
            .encode_into(out),
        NOfBytes::Two => u16::try_from(actual_length)
            .expect("Actual length should be smaller than u16 MAX_LENGTH")
            .encode_into(out),
        NOfBytes::Four => u32::try_from(actual_length)
            .expect("Actual length should be smaller than u32 MAX_LENGTH")
            .encode_into(out),
        NOfBytes::Eight => u64::try_from(actual_length)
            .expect("Actual length should be smaller than u64 MAX_LENGTH")
            .encode_into(out),
    }
}

fn decode_length_prefix<const MAX_LENGTH: usize>(
    input: &[u8],
) -> Result<(&[u8], usize), DecodeError> {
    match length_prefix_width::<MAX_LENGTH>() {
        NOfBytes::One => u8::decode(input, &()).map(|(rest, len)| (rest, usize::from(len))),
        NOfBytes::Two => u16::decode(input, &()).map(|(rest, len)| (rest, usize::from(len))),
        NOfBytes::Four => u32::decode(input, &()).map(|(rest, len)| {
            (
                rest,
                len.try_into().expect("usize should be able to hold u32"),
            )
        }),
        NOfBytes::Eight => u64::decode(input, &()).map(|(rest, len)| {
            (
                rest,
                len.try_into().expect("usize should be able to hold u64"),
            )
        }),
    }
}

impl<T, const MIN: usize, const MAX: usize> WireEncode for BoundedVec<T, MIN, MAX>
where
    T: WireEncode,
{
    fn encoded_length(&self) -> usize {
        length_prefix_len::<MAX>()
            .checked_add(self.iter().map(WireEncode::encoded_length).sum::<usize>())
            .expect("Encoded length overflow")
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        encode_length_prefix_into::<MAX>(self.len(), out);
        for item in self.iter() {
            item.encode_into(out);
        }
    }
}

impl<T, const MIN: usize, const MAX: usize> WireDecode for BoundedVec<T, MIN, MAX>
where
    T: WireDecode,
{
    type Context = T::Context;

    fn decode<'input>(
        input: &'input [u8],
        context: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (mut rest, len) = decode_length_prefix::<MAX>(input)?;

        // Check the length before decoding, so an oversized prefix never causes
        // us to decode a too-large payload.
        if len < MIN || len > MAX {
            return Err(DecodeError::length_out_of_bounds::<Self>(len, MIN, MAX));
        }

        let mut items = Vec::with_capacity(len);
        for _ in 0..len {
            let (next, item) = T::decode(rest, context)?;
            rest = next;
            items.push(item);
        }
        Ok((rest, Self::new_unchecked(items)))
    }
}

impl<T, const MIN: usize, const MAX: usize> sealed::Sealed for BoundedVec<T, MIN, MAX> where
    T: WireExamples
{
}

// The fixture is derived from the element's fixture, giving *every*
// `BoundedVec` monomorphization compile-time fixture existence for free. It
// reuses `encode_length_prefix_into`, so it is circular w.r.t. the length
// prefix — prefix drift is covered by the hand-pinned `#[test]`s below.
//
// `MIN` may be 0 (`UpperBoundedVec`), so we force at least one element;
// otherwise the fixture would be empty and never touch `T`'s codec.
impl<T, const MIN: usize, const MAX: usize> WireExamples for BoundedVec<T, MIN, MAX>
where
    T: WireExamples,
{
    fn fixtures() -> WireFixtures<Self> {
        let count = MIN.max(1);

        let mut values = Vec::with_capacity(count);
        let mut bytes = Vec::new();
        encode_length_prefix_into::<MAX>(count, &mut bytes);
        for _ in 0..count {
            let item = T::fixtures()
                .into_iter()
                .next()
                .expect("`WireExamples::fixtures` is non-empty");
            bytes.extend_from_slice(item.bytes.as_ref());
            values.push(item.value);
        }

        [WireFixture {
            value: Self::new_unchecked(values),
            bytes: Cow::Owned(bytes),
        }]
        .into()
    }
}

#[cfg(test)]
mod tests {
    use lb_utils::bounded::BoundedVec;

    use crate::{DecodeError, WireDecodeExt as _, WireEncode as _};

    /// Bound used across the tests: between 2 and 4 elements.
    const MIN: usize = 2;
    const MAX: usize = 4;

    type Bounded = BoundedVec<u8, MIN, MAX>;

    /// Builds a `BoundedVec` for encoding tests, bypassing the length checks so
    /// the codec itself remains the thing under test.
    fn bounded(items: &[u8]) -> Bounded {
        Bounded::new_unchecked(items.to_vec())
    }

    #[test]
    fn encode_prepends_a_single_byte_length_prefix() {
        assert_eq!(bounded(&[1, 2, 3]).encode_to_vec(), vec![3, 1, 2, 3]);
    }

    #[test]
    fn encode_at_the_min_and_max_lengths() {
        assert_eq!(bounded(&[1, 2]).encode_to_vec(), vec![2, 1, 2]);
        assert_eq!(bounded(&[1, 2, 3, 4]).encode_to_vec(), vec![4, 1, 2, 3, 4]);
    }

    #[test]
    fn decode_reads_a_well_formed_payload() {
        let (rest, bv) = Bounded::decode(&[3, 1, 2, 3]).unwrap();
        assert!(rest.is_empty());
        assert_eq!(bv.as_slice(), &[1, 2, 3]);
    }

    #[test]
    fn decode_leaves_trailing_bytes_untouched() {
        let (rest, bv) = Bounded::decode(&[2, 1, 2, 99, 100]).unwrap();
        assert_eq!(rest, &[99, 100]);
        assert_eq!(bv.as_slice(), &[1, 2]);
    }

    #[test]
    fn decode_at_the_min_and_max_lengths() {
        let (_, at_min) = Bounded::decode(&[2, 1, 2]).unwrap();
        assert_eq!(at_min.as_slice(), &[1, 2]);

        let (_, at_max) = Bounded::decode(&[4, 1, 2, 3, 4]).unwrap();
        assert_eq!(at_max.as_slice(), &[1, 2, 3, 4]);
    }

    #[test]
    fn decode_rejects_a_length_below_min() {
        let err = Bounded::decode(&[1, 7]).unwrap_err();
        assert!(matches!(err, DecodeError::LengthOutOfBounds { len: 1, .. }));
    }

    #[test]
    fn decode_rejects_a_zero_length() {
        let err = Bounded::decode(&[0]).unwrap_err();
        assert!(matches!(err, DecodeError::LengthOutOfBounds { len: 0, .. }));
    }

    #[test]
    fn decode_rejects_a_length_above_max() {
        let err = Bounded::decode(&[5, 1, 2, 3, 4, 5]).unwrap_err();
        assert!(matches!(err, DecodeError::LengthOutOfBounds { len: 5, .. }));
    }

    #[test]
    fn decode_rejects_an_oversized_length_even_without_a_payload() {
        let err = Bounded::decode(&[5]).unwrap_err();
        assert!(matches!(err, DecodeError::LengthOutOfBounds { len: 5, .. }));
    }

    #[test]
    fn decode_fails_on_an_empty_input() {
        let err = Bounded::decode(&[]).unwrap_err();
        assert!(matches!(err, DecodeError::UnexpectedEnd { .. }));
    }

    #[test]
    fn decode_fails_when_the_length_prefix_is_truncated() {
        // A 2-byte prefix is expected (via a `u16::MAX` maximum), but only 1
        // byte is available.
        type WideCodec = BoundedVec<u8, MIN, { u16::MAX as usize }>;
        let err = WideCodec::decode(&[0]).unwrap_err();
        assert!(matches!(err, DecodeError::UnexpectedEnd { .. }));
    }

    #[test]
    fn decode_fails_when_the_payload_is_truncated() {
        let err = Bounded::decode(&[3, 1]).unwrap_err();
        assert!(matches!(err, DecodeError::UnexpectedEnd { .. }));
    }

    #[test]
    fn encode_then_decode_roundtrips() {
        let original = bounded(&[10, 20, 30, 40]);
        let bytes = original.encode_to_vec();
        let (rest, decoded) = Bounded::decode(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!(decoded, original);
    }

    #[test]
    fn roundtrips_with_a_multi_byte_item_type() {
        type U16Codec = BoundedVec<u16, MIN, MAX>;
        let original: U16Codec = U16Codec::new_unchecked(vec![0x0102, 0x0304, 0xABCD]);

        let bytes = original.encode_to_vec();
        // 1-byte length prefix (3) (MAX == 4) followed by three little-endian `u16`s.
        assert_eq!(bytes, vec![3, 0x02, 0x01, 0x04, 0x03, 0xCD, 0xAB]);

        let (rest, decoded) = U16Codec::decode(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!(decoded, original);
    }

    #[test]
    fn two_byte_length_prefix() {
        type TwoByteBounded = BoundedVec<u8, 1, { u16::MAX as usize }>;
        let original: TwoByteBounded = [10].into();
        let bytes = original.encode_to_vec();
        assert_eq!(bytes, vec![1, 0, 10]); // 2-byte length prefix (1) then a single `u8` (10)
        let (rest, decoded) = TwoByteBounded::decode(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!(decoded, original);
    }

    #[test]
    fn four_byte_length_prefix() {
        type FourByteBounded = BoundedVec<u8, 1, { u32::MAX as usize }>;
        let original: FourByteBounded = FourByteBounded::new_unchecked(vec![10]);
        let bytes = original.encode_to_vec();
        assert_eq!(bytes, vec![1, 0, 0, 0, 10]); // 4-byte length prefix (1) then a single `u8` (10)
        let (rest, decoded) = FourByteBounded::decode(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!(decoded, original);
    }

    #[test]
    fn eight_byte_length_prefix() {
        type EightByteBounded = BoundedVec<u8, 1, { u64::MAX as usize }>;
        let original: EightByteBounded = EightByteBounded::new_unchecked(vec![10]);
        let bytes = original.encode_to_vec();
        assert_eq!(bytes, vec![1, 0, 0, 0, 0, 0, 0, 0, 10]); // 8-byte prefix (1) then a single `u8`
        let (rest, decoded) = EightByteBounded::decode(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!(decoded, original);
    }
}
