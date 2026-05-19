use lb_utils::bounded_vec::BoundedVec;
use nom::{
    IResult, Parser as _,
    bytes::take,
    combinator::{map, map_res},
    error::{Error, ErrorKind},
    multi::count,
    number::complete::u8,
};

use crate::mantle::{
    encoding::{decode_uint32, encode_uint32},
    ops::channel::{ChannelId, Ed25519PublicKey, MsgId},
};

pub trait NomEncode {
    // TODO: This could be turned into a `BoundedVec<u8, MAX_BYTES>` if we are
    // always able to set an upper limit on everything that goes through NOM
    // decoding. That would allow us to set an upper bound on ANY nom-encoded
    // struct, including a mantle tx itself.
    fn encode(&self) -> Vec<u8>;
}

pub trait NomDecode {
    type Output;

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self::Output>;
}

impl NomEncode for u8 {
    fn encode(&self) -> Vec<u8> {
        vec![*self]
    }
}

impl NomDecode for u8 {
    type Output = Self;

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self::Output> {
        u8(bytes)
    }
}

impl NomEncode for u32 {
    fn encode(&self) -> Vec<u8> {
        encode_uint32(*self)
    }
}

impl NomDecode for u32 {
    type Output = Self;

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self::Output> {
        decode_uint32(bytes)
    }
}

// Simple utility to encode a slice of `NomEncode` items by encoding each item
// and concatenating the results. Not implemented on the slice type directly
// `[T]` since that could be misleading.
fn encode_slice<T: NomEncode>(items: &[T]) -> Vec<u8> {
    items.iter().flat_map(NomEncode::encode).collect()
}

pub struct NomArray<'a, T, const N: usize>(&'a [T::Output; N])
where
    T: NomDecode;

impl<'a, T, const N: usize> From<&'a [T::Output; N]> for NomArray<'a, T, N>
where
    T: NomDecode,
{
    fn from(array: &'a [T::Output; N]) -> Self {
        Self(array)
    }
}

impl<T, const N: usize> NomEncode for NomArray<'_, T, N>
where
    T: NomDecode<Output: NomEncode>,
{
    fn encode(&self) -> Vec<u8> {
        encode_slice(self.0.as_slice())
    }
}

impl<T, const N: usize> NomDecode for NomArray<'_, T, N>
where
    T: NomDecode,
{
    type Output = [T::Output; N];

    fn decode(input: &[u8]) -> IResult<&[u8], Self::Output> {
        let (input, items) = count(T::decode, N).parse(input)?;

        let Ok(items) = items.try_into() else {
            panic!("Decoded `N` elements.");
        };
        Ok((input, items))
    }
}

pub struct NomBoundedVec<'a, T, const N: usize, const N_BYTES: usize>(&'a BoundedVec<T::Output, N>)
where
    T: NomDecode;

impl<T, const N: usize, const N_BYTES: usize> NomBoundedVec<'_, T, N, N_BYTES>
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
        assert!(N as u64 <= max_repr, "N exceeds what N_BYTES can encode");
    };
}

impl<'a, T, const N: usize, const N_BYTES: usize> From<&'a BoundedVec<T::Output, N>>
    for NomBoundedVec<'a, T, N, N_BYTES>
where
    T: NomDecode,
{
    fn from(vec: &'a BoundedVec<T::Output, N>) -> Self {
        let () = Self::_N_BYTES_VALUE_CHECK;

        Self(vec)
    }
}

impl<T, const N: usize, const N_BYTES: usize> NomEncode for NomBoundedVec<'_, T, N, N_BYTES>
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

impl<T, const N: usize, const N_BYTES: usize> NomDecode for NomBoundedVec<'_, T, N, N_BYTES>
where
    T: NomDecode,
{
    type Output = BoundedVec<T::Output, N>;

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self::Output> {
        let () = Self::_N_BYTES_VALUE_CHECK;

        let (bytes, len_bytes): (&[u8], &[u8]) = take(N_BYTES).parse(bytes)?;
        let mut buf = [0u8; 8];
        buf[..N_BYTES].copy_from_slice(len_bytes);
        let len = u64::from_le_bytes(buf) as usize;

        if len > N {
            return Err(nom::Err::Error(Error::new(bytes, ErrorKind::TooLarge)));
        }

        let (bytes, items) = count(T::decode, len).parse(bytes)?;
        Ok((bytes, BoundedVec::new_unchecked(items)))
    }
}

impl NomEncode for ChannelId {
    fn encode(&self) -> Vec<u8> {
        encode_slice(self.as_ref())
    }
}

impl NomDecode for ChannelId {
    type Output = Self;

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        map(NomArray::<u8, 32>::decode, Self::from).parse(bytes)
    }
}

impl NomEncode for MsgId {
    fn encode(&self) -> Vec<u8> {
        encode_slice(self.as_ref())
    }
}

impl NomDecode for MsgId {
    type Output = Self;

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        map(NomArray::<u8, 32>::decode, Self::from).parse(bytes)
    }
}

// Ed25519PublicKey = 32BYTE
impl NomEncode for Ed25519PublicKey {
    fn encode(&self) -> Vec<u8> {
        encode_slice(&self.to_bytes())
    }
}

impl NomDecode for Ed25519PublicKey {
    type Output = Self;

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        map_res(NomArray::<u8, 32>::decode, |key_bytes: [u8; 32]| {
            Self::from_bytes(&key_bytes).map_err(|_| Error::new(bytes, ErrorKind::Fail))
        })
        .parse(bytes)
    }
}
