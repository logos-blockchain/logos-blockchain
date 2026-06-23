use lb_utils::bounded_vec::BoundedVec;
use nom::{
    IResult, Parser as _,
    error::{Error, ErrorKind},
    multi::count,
};

use crate::mantle::nom::{NomDecode, NomEncode, encode_slice};

#[repr(usize)]
enum NOfBytes {
    One = 1,
    Two = 2,
    Four = 4,
    Eight = 8,
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
        NOfBytes::One => (actual_length as u8).encode(),
        NOfBytes::Two => (actual_length as u16).encode(),
        NOfBytes::Four => (actual_length as u32).encode(),
        NOfBytes::Eight => (actual_length as u64).encode(),
    }
}

fn decode_length_prefix<const MAX_LENGTH: usize>(bytes: &[u8]) -> IResult<&[u8], usize> {
    match length_prefix_width::<MAX_LENGTH>() {
        NOfBytes::One => u8::decode(bytes).map(|(rest, len)| (rest, len as usize)),
        NOfBytes::Two => u16::decode(bytes).map(|(rest, len)| (rest, len as usize)),
        NOfBytes::Four => u32::decode(bytes).map(|(rest, len)| (rest, len as usize)),
        NOfBytes::Eight => u64::decode(bytes).map(|(rest, len)| (rest, len as usize)),
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
    use lb_utils::bounded_vec::BoundedVec as BV;
    use nom::error::ErrorKind;

    use crate::mantle::nom::{NomDecode as _, NomEncode as _};

    /// Bound used across the tests: between 2 and 4 elements.
    const MIN: usize = 2;
    const MAX: usize = 4;

    type BoundedVec = BV<u8, MIN, MAX>;

    /// Builds a `BoundedVec` for encoding tests, bypassing the length checks so
    /// the codec itself remains the thing under test.
    fn bounded(items: &[u8]) -> BoundedVec {
        BoundedVec::new_unchecked(items.to_vec())
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
    fn encode_prefix_width_follows_n_bytes() {
        // The length prefix is `N_BYTES` little-endian bytes wide.
        assert_eq!(bounded(&[1, 2, 3]).encode(), vec![3, 1, 2, 3]);
        assert_eq!(bounded(&[1, 2, 3]).encode(), vec![3, 1, 2, 3]);
        assert_eq!(bounded(&[1, 2, 3]).encode(), vec![3, 1, 2, 3]);
    }

    #[test]
    fn encode_at_the_min_and_max_lengths() {
        assert_eq!(bounded(&[1, 2]).encode(), vec![2, 1, 2]);
        assert_eq!(bounded(&[1, 2, 3, 4]).encode(), vec![4, 1, 2, 3, 4]);
    }

    #[test]
    fn decode_reads_a_well_formed_payload() {
        let (rest, bv) = BoundedVec::decode(&[3, 1, 2, 3]).unwrap();
        assert!(rest.is_empty());
        assert_eq!(bv.as_slice(), &[1, 2, 3]);
    }

    #[test]
    fn decode_leaves_trailing_bytes_untouched() {
        // Only the prefix and `len` items are consumed; the rest is returned.
        let (rest, bv) = BoundedVec::decode(&[2, 1, 2, 99, 100]).unwrap();
        assert_eq!(rest, &[99, 100]);
        assert_eq!(bv.as_slice(), &[1, 2]);
    }

    #[test]
    fn decode_at_the_min_and_max_lengths() {
        let (_, at_min) = BoundedVec::decode(&[2, 1, 2]).unwrap();
        assert_eq!(at_min.as_slice(), &[1, 2]);

        let (_, at_max) = BoundedVec::decode(&[4, 1, 2, 3, 4]).unwrap();
        assert_eq!(at_max.as_slice(), &[1, 2, 3, 4]);
    }

    #[test]
    fn decode_rejects_a_length_below_min() {
        // `len == 1 < MIN`: rejected before any payload is consumed.
        let err = BoundedVec::decode(&[1, 7]).unwrap_err();
        assert_eq!(error_kind(err), ErrorKind::LengthValue);
    }

    #[test]
    fn decode_rejects_a_zero_length() {
        let err = BoundedVec::decode(&[0]).unwrap_err();
        assert_eq!(error_kind(err), ErrorKind::LengthValue);
    }

    #[test]
    fn decode_rejects_a_length_above_max() {
        // `len == 5 > MAX`: rejected up front, so the oversized payload that
        // would follow is never decoded.
        let err = BoundedVec::decode(&[5, 1, 2, 3, 4, 5]).unwrap_err();
        assert_eq!(error_kind(err), ErrorKind::TooLarge);
    }

    #[test]
    fn decode_rejects_an_oversized_length_even_without_a_payload() {
        // The length check happens before items are read, so a bogus prefix
        // alone is enough to fail.
        let err = BoundedVec::decode(&[5]).unwrap_err();
        assert_eq!(error_kind(err), ErrorKind::TooLarge);
    }

    #[test]
    fn decode_fails_on_an_empty_input() {
        // Not even the length prefix can be read.
        assert!(BoundedVec::decode(&[]).is_err());
    }

    #[test]
    fn decode_fails_when_the_payload_is_truncated() {
        // The prefix promises 3 items but only 1 byte follows.
        let err = BoundedVec::decode(&[3, 1]).unwrap_err();
        assert!(matches!(err, nom::Err::Error(_) | nom::Err::Failure(_)));
    }

    #[test]
    fn encode_then_decode_roundtrips() {
        let original = bounded(&[10, 20, 30, 40]);
        let bytes = original.encode();
        let (rest, decoded) = BoundedVec::decode(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!(decoded, original);
    }

    #[test]
    fn roundtrips_with_a_multi_byte_item_type() {
        type U16Codec = BV<u16, MIN, MAX>;
        let original: U16Codec = U16Codec::new_unchecked(vec![0x0102, 0x0304, 0xABCD]);

        let bytes = original.encode();
        // 2-byte length prefix (3) followed by three little-endian `u16`s.
        assert_eq!(bytes, vec![3, 0, 0x02, 0x01, 0x04, 0x03, 0xCD, 0xAB]);

        let (rest, decoded) = U16Codec::decode(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!(decoded, original);
    }
}
