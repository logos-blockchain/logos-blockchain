use lb_utils::bounded_vec::BoundedVec;
use nom::{
    IResult, Parser as _,
    error::{Error, ErrorKind},
    multi::count,
};

use crate::mantle::nom::{NomDecode, NomEncode, encode_slice};

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

fn encode_length_prefix<const MAX_LENGTH: usize>(actual_length: usize) -> Vec<u8> {
    match length_prefix_width::<MAX_LENGTH>() {
        // Encode as `u8`
        NOfBytes::One => (u8::try_from(actual_length)
            .expect("Actual length should be smaller than u8 MAX_LENGTH"))
        .encode(),
        // Encode as `u16`
        NOfBytes::Two => (u16::try_from(actual_length)
            .expect("Actual length should be smaller than u16 MAX_LENGTH"))
        .encode(),
        // Encode as `u32`
        NOfBytes::Four => (u32::try_from(actual_length)
            .expect("Actual length should be smaller than u32 MAX_LENGTH"))
        .encode(),
        // Encode as `u64`
        NOfBytes::Eight => (u64::try_from(actual_length)
            .expect("Actual length should be smaller than u64 MAX_LENGTH"))
        .encode(),
    }
}

fn decode_length_prefix<const MAX_LENGTH: usize>(bytes: &[u8]) -> IResult<&[u8], usize> {
    match length_prefix_width::<MAX_LENGTH>() {
        NOfBytes::One => u8::decode(bytes).map(|(rest, len)| (rest, len.into())),
        NOfBytes::Two => u16::decode(bytes).map(|(rest, len)| (rest, len.into())),
        NOfBytes::Four => u32::decode(bytes).map(|(rest, len)| {
            (
                rest,
                len.try_into().expect("usize should be able to hold u32"),
            )
        }),
        NOfBytes::Eight => u64::decode(bytes).map(|(rest, len)| {
            (
                rest,
                len.try_into().expect("usize should be able to hold u64"),
            )
        }),
    }
}

impl<T, const MIN: usize, const MAX: usize> NomEncode for BoundedVec<T, MIN, MAX>
where
    T: NomEncode,
{
    fn encode(&self) -> Vec<u8> {
        let mut bytes = encode_length_prefix::<MAX>(self.len());
        bytes.extend(encode_slice(self.as_slice()));

        bytes
    }
}

impl<T, const MIN: usize, const MAX: usize> NomDecode for BoundedVec<T, MIN, MAX>
where
    T: NomDecode,
{
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        let (bytes, len) = decode_length_prefix::<MAX>(bytes)?;

        // We check length first instead of relying on `BoundedVec::try_from` to avoid
        // decoding a payload that is too large.
        if len < MIN {
            return Err(nom::Err::Error(Error::new(bytes, ErrorKind::LengthValue)));
        }
        if len > MAX {
            return Err(nom::Err::Error(Error::new(bytes, ErrorKind::TooLarge)));
        }

        let (bytes, items) = count(T::decode, len).parse_complete(bytes)?;
        Ok((bytes, Self::new_unchecked(items)))
    }
}

#[cfg(test)]
mod tests {
    use lb_utils::bounded_vec::BoundedVec;
    use nom::error::ErrorKind;

    use crate::mantle::nom::{NomDecode as _, NomEncode as _};

    /// Bound used across the tests: between 2 and 4 elements.
    const MIN: usize = 2;
    const MAX: usize = 4;

    type Bounded = BoundedVec<u8, MIN, MAX>;

    /// Builds a `BoundedVec` for encoding tests, bypassing the length checks so
    /// the codec itself remains the thing under test.
    fn bounded(items: &[u8]) -> Bounded {
        Bounded::new_unchecked(items.to_vec())
    }

    /// Extracts the [`ErrorKind`] from a nom decode error.
    fn error_kind(err: nom::Err<nom::error::Error<&[u8]>>) -> ErrorKind {
        match err {
            nom::Err::Error(e) | nom::Err::Failure(e) => e.code,
            nom::Err::Incomplete(_) => panic!("unexpected incomplete error"),
        }
    }

    #[test]
    fn encode_prepends_a_single_byte_length_prefix() {
        assert_eq!(bounded(&[1, 2, 3]).encode(), vec![3, 1, 2, 3]);
    }

    #[test]
    fn encode_at_the_min_and_max_lengths() {
        assert_eq!(bounded(&[1, 2]).encode(), vec![2, 1, 2]);
        assert_eq!(bounded(&[1, 2, 3, 4]).encode(), vec![4, 1, 2, 3, 4]);
    }

    #[test]
    fn decode_reads_a_well_formed_payload() {
        let (rest, bv) = Bounded::decode(&[3, 1, 2, 3]).unwrap();
        assert!(rest.is_empty());
        assert_eq!(bv.as_slice(), &[1, 2, 3]);
    }

    #[test]
    fn decode_leaves_trailing_bytes_untouched() {
        // Only the prefix and `len` items are consumed; the rest is returned.
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
        // `len == 1 < MIN`: rejected before any payload is consumed.
        let err = Bounded::decode(&[1, 7]).unwrap_err();
        assert_eq!(error_kind(err), ErrorKind::LengthValue);
    }

    #[test]
    fn decode_rejects_a_zero_length() {
        let err = Bounded::decode(&[0]).unwrap_err();
        assert_eq!(error_kind(err), ErrorKind::LengthValue);
    }

    #[test]
    fn decode_rejects_a_length_above_max() {
        // `len == 5 > MAX`: rejected up front, so the oversized payload that
        // would follow is never decoded.
        let err = Bounded::decode(&[5, 1, 2, 3, 4, 5]).unwrap_err();
        assert_eq!(error_kind(err), ErrorKind::TooLarge);
    }

    #[test]
    fn decode_rejects_an_oversized_length_even_without_a_payload() {
        // The length check happens before items are read, so a bogus prefix
        // alone is enough to fail.
        let err = Bounded::decode(&[5]).unwrap_err();
        assert_eq!(error_kind(err), ErrorKind::TooLarge);
    }

    #[test]
    fn decode_fails_on_an_empty_input() {
        // Not even the length prefix can be read.
        assert!(Bounded::decode(&[]).is_err());
    }

    #[test]
    fn decode_fails_when_the_length_prefix_is_truncated() {
        // A 2-byte prefix is expected (by using a vec with `u16::MAX` as the maximum
        // length), but only 1 byte is available.
        type WideCodec = BoundedVec<u8, MIN, { u16::MAX as usize }>;
        assert!(WideCodec::decode(&[0]).is_err());
    }

    #[test]
    fn decode_fails_when_the_payload_is_truncated() {
        // The prefix promises 3 items but only 1 byte follows.
        let err = Bounded::decode(&[3, 1]).unwrap_err();
        assert!(matches!(err, nom::Err::Error(_) | nom::Err::Failure(_)));
    }

    #[test]
    fn encode_then_decode_roundtrips() {
        let original = bounded(&[10, 20, 30, 40]);
        let bytes = original.encode();
        let (rest, decoded) = Bounded::decode(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!(decoded, original);
    }

    #[test]
    fn roundtrips_with_a_multi_byte_item_type() {
        type U16Codec = BoundedVec<u16, MIN, MAX>;
        let original: U16Codec = U16Codec::new_unchecked(vec![0x0102, 0x0304, 0xABCD]);

        let bytes = original.encode();
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
        let bytes = original.encode();
        assert_eq!(bytes, vec![1, 0, 10]); // 2-byte length prefix (1) followed by a single `u8` (10)
        let (rest, decoded) = TwoByteBounded::decode(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!(decoded, original);
    }

    #[test]
    fn four_byte_length_prefix() {
        type FourByteBounded = BoundedVec<u8, 1, { u32::MAX as usize }>;
        let original: FourByteBounded = FourByteBounded::new_unchecked(vec![10]);
        let bytes = original.encode();
        assert_eq!(bytes, vec![1, 0, 0, 0, 10]); // 4-byte length prefix (1) followed by a single `u8` (10)
        let (rest, decoded) = FourByteBounded::decode(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!(decoded, original);
    }

    #[test]
    fn eight_byte_length_prefix() {
        type EightByteBounded = BoundedVec<u8, 1, { u64::MAX as usize }>;
        let original: EightByteBounded = EightByteBounded::new_unchecked(vec![10]);
        let bytes = original.encode();
        assert_eq!(bytes, vec![1, 0, 0, 0, 0, 0, 0, 0, 10]); // 8-byte length prefix (1) followed by a single `u8` (10)
        let (rest, decoded) = EightByteBounded::decode(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!(decoded, original);
    }
}
