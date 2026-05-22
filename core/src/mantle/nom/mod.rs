use nom::{
    IResult, Parser as _,
    combinator::{map, map_res},
    error::{Error, ErrorKind},
    number::complete::{le_u16, le_u32, u8},
};

use crate::mantle::ops::channel::{ChannelId, Ed25519PublicKey, MsgId};

pub mod array;
pub use self::array::NomArray;
pub mod bounded_vec;
pub use self::bounded_vec::NomBoundedVec;

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

impl NomEncode for u16 {
    fn encode(&self) -> Vec<u8> {
        self.to_le_bytes().to_vec()
    }
}

impl NomDecode for u16 {
    type Output = Self;

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self::Output> {
        le_u16(bytes)
    }
}

impl NomEncode for u32 {
    fn encode(&self) -> Vec<u8> {
        self.to_le_bytes().to_vec()
    }
}

impl NomDecode for u32 {
    type Output = Self;

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self::Output> {
        le_u32(bytes)
    }
}

// Simple utility to encode a slice of `NomEncode` items by encoding each item
// and concatenating the results. Not implemented on the slice type directly
// `[T]` since that could be misleading.
fn encode_slice<T: NomEncode>(items: &[T]) -> Vec<u8> {
    items.iter().flat_map(NomEncode::encode).collect()
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
