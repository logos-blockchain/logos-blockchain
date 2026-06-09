use lb_utils::bounded_vec::BoundedVec;
use nom::{
    IResult, Parser as _,
    bytes::complete::take,
    error::{Error, ErrorKind},
    multi::count,
};

use crate::mantle::nom::{NomDecode, NomEncode, encode_slice};

/// Nom encoder for bounded vectors with a specified number of bytes for the
/// length prefix.
pub struct NomBoundedVec<'a, T, const MIN: usize, const MAX: usize, const N_BYTES: usize>(
    &'a BoundedVec<T::Output, MIN, MAX>,
)
where
    T: NomDecode;

impl<T, const MIN: usize, const MAX: usize, const N_BYTES: usize>
    NomBoundedVec<'_, T, MIN, MAX, N_BYTES>
where
    T: NomDecode,
{
    const _N_BYTES_VALUE_CHECK: () = {
        assert!(
            matches!(N_BYTES, 1 | 2 | 4 | 8),
            "N_BYTES must be 1, 2, 4, or 8",
        );
        let max_repr: u64 = if N_BYTES == 8 {
            u64::MAX
        } else {
            (1u64 << (N_BYTES * 8)) - 1
        };
        assert!(
            MAX as u64 <= max_repr,
            "MAX exceeds what N_BYTES can encode"
        );
    };
}

impl<'a, T, const MIN: usize, const MAX: usize, const N_BYTES: usize>
    From<&'a BoundedVec<T::Output, MIN, MAX>> for NomBoundedVec<'a, T, MIN, MAX, N_BYTES>
where
    T: NomDecode,
{
    fn from(vec: &'a BoundedVec<T::Output, MIN, MAX>) -> Self {
        let () = Self::_N_BYTES_VALUE_CHECK;

        Self(vec)
    }
}

impl<T, const MIN: usize, const MAX: usize, const N_BYTES: usize> NomEncode
    for NomBoundedVec<'_, T, MIN, MAX, N_BYTES>
where
    T: NomDecode<Output: NomEncode>,
{
    fn encode(&self) -> Vec<u8> {
        let () = Self::_N_BYTES_VALUE_CHECK;

        // Initialize `bytes` with the encoded length prefix.
        let mut bytes = (self.0.len() as u64).to_le_bytes()[..N_BYTES].to_vec();
        bytes.extend(encode_slice(self.0.as_slice()));

        bytes
    }
}

impl<T, const MIN: usize, const MAX: usize, const N_BYTES: usize> NomDecode
    for NomBoundedVec<'_, T, MIN, MAX, N_BYTES>
where
    T: NomDecode,
{
    type Output = BoundedVec<T::Output, MIN, MAX>;

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self::Output> {
        let () = Self::_N_BYTES_VALUE_CHECK;

        let (bytes, len_bytes): (&[u8], &[u8]) = take(N_BYTES).parse(bytes)?;
        let mut buf = [0u8; 8];
        buf[..N_BYTES].copy_from_slice(len_bytes);
        let len = u64::from_le_bytes(buf) as usize;

        // We check length first instead of relying on `BoundedVec::try_from` to avoid
        // decoding a payload that is too large.
        if len < MIN {
            return Err(nom::Err::Error(Error::new(bytes, ErrorKind::LengthValue)));
        }
        if len > MAX {
            return Err(nom::Err::Error(Error::new(bytes, ErrorKind::TooLarge)));
        }

        let (bytes, items) = count(T::decode, len).parse(bytes)?;
        Ok((bytes, BoundedVec::new_unchecked(items)))
    }
}

#[cfg(test)]
mod tests {
    use lb_utils::bounded_vec::BoundedVec;
    use nom::error::ErrorKind;

    use crate::mantle::nom::{NomBoundedVec, NomDecode as _, NomEncode as _};

    /// Bound used across the tests: between 2 and 4 elements.
    const MIN: usize = 2;
    const MAX: usize = 4;

    /// `NomBoundedVec` of `u8` items with a single-byte length prefix.
    type Bounded = BoundedVec<u8, MIN, MAX>;
    type Codec = NomBoundedVec<'static, u8, { Bounded::MIN }, { Bounded::MAX }, 1>;

    /// Builds a `BoundedVec` for encoding tests, bypassing the length checks so
    /// the codec itself remains the thing under test.
    fn bounded(items: &[u8]) -> Bounded {
        BoundedVec::new_unchecked(items.to_vec())
    }

    /// Encodes a borrowed `BoundedVec` through `NomBoundedVec` with the given
    /// length-prefix width.
    fn encode<const N_BYTES: usize>(items: &[u8]) -> Vec<u8> {
        let bv = bounded(items);
        let codec: NomBoundedVec<'_, u8, MIN, MAX, N_BYTES> = (&bv).into();
        codec.encode()
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
        assert_eq!(encode::<1>(&[1, 2, 3]), vec![3, 1, 2, 3]);
    }

    #[test]
    fn encode_prefix_width_follows_n_bytes() {
        // The length prefix is `N_BYTES` little-endian bytes wide.
        assert_eq!(encode::<2>(&[1, 2, 3]), vec![3, 0, 1, 2, 3]);
        assert_eq!(encode::<4>(&[1, 2, 3]), vec![3, 0, 0, 0, 1, 2, 3]);
        assert_eq!(
            encode::<8>(&[1, 2, 3]),
            vec![3, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3]
        );
    }

    #[test]
    fn encode_at_the_min_and_max_lengths() {
        assert_eq!(encode::<1>(&[1, 2]), vec![2, 1, 2]);
        assert_eq!(encode::<1>(&[1, 2, 3, 4]), vec![4, 1, 2, 3, 4]);
    }

    #[test]
    fn decode_reads_a_well_formed_payload() {
        let (rest, bv) = Codec::decode(&[3, 1, 2, 3]).unwrap();
        assert!(rest.is_empty());
        assert_eq!(bv.as_slice(), &[1, 2, 3]);
    }

    #[test]
    fn decode_leaves_trailing_bytes_untouched() {
        // Only the prefix and `len` items are consumed; the rest is returned.
        let (rest, bv) = Codec::decode(&[2, 1, 2, 99, 100]).unwrap();
        assert_eq!(rest, &[99, 100]);
        assert_eq!(bv.as_slice(), &[1, 2]);
    }

    #[test]
    fn decode_at_the_min_and_max_lengths() {
        let (_, at_min) = Codec::decode(&[2, 1, 2]).unwrap();
        assert_eq!(at_min.as_slice(), &[1, 2]);

        let (_, at_max) = Codec::decode(&[4, 1, 2, 3, 4]).unwrap();
        assert_eq!(at_max.as_slice(), &[1, 2, 3, 4]);
    }

    #[test]
    fn decode_rejects_a_length_below_min() {
        // `len == 1 < MIN`: rejected before any payload is consumed.
        let err = Codec::decode(&[1, 7]).unwrap_err();
        assert_eq!(error_kind(err), ErrorKind::LengthValue);
    }

    #[test]
    fn decode_rejects_a_zero_length() {
        let err = Codec::decode(&[0]).unwrap_err();
        assert_eq!(error_kind(err), ErrorKind::LengthValue);
    }

    #[test]
    fn decode_rejects_a_length_above_max() {
        // `len == 5 > MAX`: rejected up front, so the oversized payload that
        // would follow is never decoded.
        let err = Codec::decode(&[5, 1, 2, 3, 4, 5]).unwrap_err();
        assert_eq!(error_kind(err), ErrorKind::TooLarge);
    }

    #[test]
    fn decode_rejects_an_oversized_length_even_without_a_payload() {
        // The length check happens before items are read, so a bogus prefix
        // alone is enough to fail.
        let err = Codec::decode(&[5]).unwrap_err();
        assert_eq!(error_kind(err), ErrorKind::TooLarge);
    }

    #[test]
    fn decode_fails_on_an_empty_input() {
        // Not even the length prefix can be read.
        assert!(Codec::decode(&[]).is_err());
    }

    #[test]
    fn decode_fails_when_the_length_prefix_is_truncated() {
        // A 4-byte prefix is expected but only 2 bytes are available.
        type WideCodec = NomBoundedVec<'static, u8, MIN, MAX, 4>;
        assert!(WideCodec::decode(&[0, 0]).is_err());
    }

    #[test]
    fn decode_fails_when_the_payload_is_truncated() {
        // The prefix promises 3 items but only 1 byte follows.
        let err = Codec::decode(&[3, 1]).unwrap_err();
        assert!(matches!(err, nom::Err::Error(_) | nom::Err::Failure(_)));
    }

    #[test]
    fn encode_then_decode_roundtrips() {
        let original = bounded(&[10, 20, 30, 40]);
        let codec: NomBoundedVec<'_, u8, MIN, MAX, 1> = (&original).into();
        let bytes = codec.encode();
        let (rest, decoded) = Codec::decode(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!(decoded, original);
    }

    #[test]
    fn roundtrips_with_a_multi_byte_item_type() {
        // `u16` items exercise per-item encoding/decoding within the codec.
        type U16Codec = NomBoundedVec<'static, u16, MIN, MAX, 2>;
        let original: BoundedVec<u16, MIN, MAX> =
            BoundedVec::new_unchecked(vec![0x0102, 0x0304, 0xABCD]);
        let codec: NomBoundedVec<'_, u16, MIN, MAX, 2> = (&original).into();

        let bytes = codec.encode();
        // 2-byte length prefix (3) followed by three little-endian `u16`s.
        assert_eq!(bytes, vec![3, 0, 0x02, 0x01, 0x04, 0x03, 0xCD, 0xAB]);

        let (rest, decoded) = U16Codec::decode(&bytes).unwrap();
        assert!(rest.is_empty());
        assert_eq!(decoded, original);
    }
}
