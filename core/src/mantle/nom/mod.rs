use core::ops::{Deref, DerefMut};

use lb_utils::bounded_vec::BoundedVec;
use nom::{
    IResult, Parser as _,
    bytes::take,
    combinator::{map, map_res},
    error::{Error, ErrorKind},
    multi::count,
    number::complete::u8,
};
use serde::{Deserialize, Serialize};

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
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self>
    where
        Self: Sized;
}

impl NomEncode for u8 {
    fn encode(&self) -> Vec<u8> {
        vec![*self]
    }

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        u8(bytes)
    }
}

impl NomEncode for u32 {
    fn encode(&self) -> Vec<u8> {
        encode_uint32(*self)
    }

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        decode_uint32(bytes)
    }
}

// Simple utility to encode a slice of `NomEncode` items by encoding each item
// and concatenating the results.
impl<T> NomEncode for [T]
where
    T: NomEncode,
{
    fn encode(&self) -> Vec<u8> {
        self.iter().flat_map(NomEncode::encode).collect()
    }
}

// Fixed-length slices are encoded without a length prefix, since the length is
// implied by the type.
impl<T, const N: usize> NomEncode for [T; N]
where
    T: NomEncode,
{
    fn encode(&self) -> Vec<u8> {
        self.as_slice().encode()
    }

    fn decode(input: &[u8]) -> IResult<&[u8], Self> {
        let (input, items) = count(T::decode, N).parse(input)?;

        let Ok(items) = items.try_into() else {
            panic!("Decoded `N` elements.");
        };
        Ok((input, items))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(bound(serialize = "T: Clone + Serialize"))]
pub struct NomBoundedVec<T, const N: usize, const N_BYTES: usize>(BoundedVec<T, N>);

impl<T, const N: usize, const N_BYTES: usize> NomBoundedVec<T, N, N_BYTES> {
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

    #[must_use]
    pub const fn new() -> Self {
        Self(BoundedVec::new())
    }

    #[must_use]
    pub const fn new_unchecked(items: Vec<T>) -> Self {
        Self(BoundedVec::new_unchecked(items))
    }
}

impl<T, const N: usize, const N_BYTES: usize> From<NomBoundedVec<T, N, N_BYTES>> for Vec<T> {
    fn from(value: NomBoundedVec<T, N, N_BYTES>) -> Self {
        value.0.into()
    }
}

impl<T, const N: usize, const N_BYTES: usize> From<BoundedVec<T, N>>
    for NomBoundedVec<T, N, N_BYTES>
{
    fn from(value: BoundedVec<T, N>) -> Self {
        Self(value)
    }
}

impl<T, const N: usize, const N_BYTES: usize> TryFrom<Vec<T>> for NomBoundedVec<T, N, N_BYTES> {
    type Error = <BoundedVec<T, N> as TryFrom<Vec<T>>>::Error;

    fn try_from(value: Vec<T>) -> Result<Self, Self::Error> {
        Ok(Self(BoundedVec::try_from(value)?))
    }
}

impl<T, const INPUT_SIZE: usize, const MAX: usize, const N_BYTES: usize> From<[T; INPUT_SIZE]>
    for NomBoundedVec<T, MAX, N_BYTES>
{
    fn from(value: [T; INPUT_SIZE]) -> Self {
        Self(value.into())
    }
}

impl<T, const INPUT_SIZE: usize, const MAX: usize, const N_BYTES: usize> From<&[T; INPUT_SIZE]>
    for NomBoundedVec<T, MAX, N_BYTES>
where
    T: Clone,
{
    fn from(value: &[T; INPUT_SIZE]) -> Self {
        Self(value.clone().into())
    }
}

impl<T, const N: usize, const N_BYTES: usize> Deref for NomBoundedVec<T, N, N_BYTES> {
    type Target = BoundedVec<T, N>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T, const N: usize, const N_BYTES: usize> DerefMut for NomBoundedVec<T, N, N_BYTES> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T, const N: usize, const N_BYTES: usize> AsRef<[T]> for NomBoundedVec<T, N, N_BYTES> {
    fn as_ref(&self) -> &[T] {
        self.0.as_ref()
    }
}

impl<T, const N: usize, const N_BYTES: usize> NomEncode for NomBoundedVec<T, N, N_BYTES>
where
    T: NomEncode,
{
    fn encode(&self) -> Vec<u8> {
        let () = Self::_N_BYTES_VALUE_CHECK;

        // Initialize `bytes` with the encoded length prefix.
        let mut bytes = (self.len() as u64).to_le_bytes()[..N_BYTES].to_vec();
        bytes.extend(self.as_slice().encode());

        bytes
    }

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        let () = Self::_N_BYTES_VALUE_CHECK;

        let (bytes, len_bytes): (&[u8], &[u8]) = take(N_BYTES).parse(bytes)?;
        let mut buf = [0u8; 8];
        buf[..N_BYTES].copy_from_slice(len_bytes);
        let len = u64::from_le_bytes(buf) as usize;

        if len > N {
            return Err(nom::Err::Error(Error::new(bytes, ErrorKind::TooLarge)));
        }

        let (bytes, items) = count(T::decode, len).parse(bytes)?;
        Ok((bytes, Self(BoundedVec::new_unchecked(items))))
    }
}

impl NomEncode for ChannelId {
    fn encode(&self) -> Vec<u8> {
        self.as_ref().encode()
    }

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        map(<[u8; 32]>::decode, Self::from).parse(bytes)
    }
}

impl NomEncode for MsgId {
    fn encode(&self) -> Vec<u8> {
        self.as_ref().encode()
    }

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        map(<[u8; 32]>::decode, Self::from).parse(bytes)
    }
}

// Ed25519PublicKey = 32BYTE
impl NomEncode for Ed25519PublicKey {
    fn encode(&self) -> Vec<u8> {
        self.to_bytes().encode()
    }

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        map_res(<[u8; 32]>::decode, |key_bytes: [u8; 32]| {
            Self::from_bytes(&key_bytes).map_err(|_| Error::new(bytes, ErrorKind::Fail))
        })
        .parse(bytes)
    }
}
