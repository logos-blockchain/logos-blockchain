//! The length prefix of a bounded collection: a little-endian count whose
//! width is fixed by the collection's `MAX`, as the smallest of 1, 2 or 4
//! bytes that can hold every legal length.
//!
//! Shared by every bounded collection codec, so they all agree on the format.

use super::{BinaryDecode as _, BinaryEncode as _, DecodeError};

/// The largest `MAX` a length prefix encodes.
///
/// A `usize` bound can differ across platforms: `usize::MAX` would take an
/// eight-byte prefix on a 64-bit node and a four-byte one on a 32-bit node, so
/// the two would disagree on the encoding. Capping bounds at `u32::MAX` makes
/// every legal bound, and with it every prefix, the same on both, and a
/// collection bounded above it fails to compile.
///
/// ```
/// use lb_utils::bounded::BoundedVec;
/// use logos_blockchain_binary_codec::canonical::BinaryEncode as _;
///
/// let widest = BoundedVec::<u8, 0, { u32::MAX as usize }>::new_unchecked(vec![7]);
/// assert_eq!(widest.encode_to_vec(), [1, 0, 0, 0, 7]);
/// ```
///
/// ```compile_fail,E0080
/// use lb_utils::bounded::BoundedVec;
/// use logos_blockchain_binary_codec::canonical::BinaryEncode as _;
///
/// let platform_sized = BoundedVec::<u8, 0, { usize::MAX }>::new_unchecked(vec![7]);
/// drop(platform_sized.encode_to_vec());
/// ```
pub const MAX_ENCODABLE_LENGTH: usize = u32::MAX as usize;

#[derive(Debug, Clone, Copy)]
enum NOfBytes {
    One,
    Two,
    Four,
}

const fn length_prefix_width<const MAX_LENGTH: usize>() -> NOfBytes {
    const {
        assert!(
            MAX_LENGTH <= MAX_ENCODABLE_LENGTH,
            "a canonically encoded collection must be bounded by at most `u32::MAX`, so that \
             its length prefix is the same on every platform"
        );
    }
    if MAX_LENGTH <= u8::MAX as usize {
        NOfBytes::One
    } else if MAX_LENGTH <= u16::MAX as usize {
        NOfBytes::Two
    } else {
        NOfBytes::Four
    }
}

/// Byte-width of the length prefix for a `MAX_LENGTH`-bounded collection.
pub(super) const fn length_prefix_len<const MAX_LENGTH: usize>() -> usize {
    match length_prefix_width::<MAX_LENGTH>() {
        NOfBytes::One => 1,
        NOfBytes::Two => 2,
        NOfBytes::Four => 4,
    }
}

pub(super) fn encode_length_prefix_into<const MAX_LENGTH: usize>(
    actual_length: usize,
    out: &mut Vec<u8>,
) {
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
    }
}

pub(super) fn decode_length_prefix<const MAX_LENGTH: usize>(
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
    }
}

/// Decodes the length prefix of a `[MIN, MAX]`-bounded `T` and checks it
/// against the bound before a single item is decoded.
pub(super) fn decode_bounded_length<T, const MIN: usize, const MAX: usize>(
    input: &[u8],
) -> Result<(&[u8], usize), DecodeError>
where
    T: ?Sized,
{
    let (rest, len) = decode_length_prefix::<MAX>(input)?;
    if len < MIN || len > MAX {
        return Err(DecodeError::length_out_of_bounds::<T>(len, MIN, MAX));
    }
    Ok((rest, len))
}
