use lb_utils::bounded_vec::BoundedVec;
use nom::{
    IResult, Parser as _,
    bytes::take,
    combinator::{map, map_res},
    error::{Error, ErrorKind},
};

use crate::mantle::{
    encoding::{decode_uint32, encode_uint32},
    ops::channel::{ChannelId, Ed25519PublicKey, MsgId},
};

pub trait NomEncode: Sized {
    // TODO: This could be turned into a `BoundedVec<u8, MAX_BYTES>` if we are
    // always able to set an upper limit on everything that goes through NOM
    // decoding.
    fn encode(&self) -> Vec<u8>;
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self>;
}

impl NomEncode for u32 {
    fn encode(&self) -> Vec<u8> {
        encode_uint32(*self)
    }

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        decode_uint32(bytes)
    }
}

impl<const N: usize> NomEncode for BoundedVec<u8, N> {
    fn encode(&self) -> Vec<u8> {
        let mut bytes = (self.len() as u32).encode();
        bytes.extend(self.as_slice());
        bytes
    }

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        let (input, len) = decode_uint32(bytes)?;

        if len > N as u32 {
            return Err(nom::Err::Error(Error::new(input, ErrorKind::TooLarge)));
        }

        let (input, bytes) = map(take(len as usize), <[u8]>::to_vec).parse(input)?;

        Ok((input, Self::new_unchecked(bytes)))
    }
}

impl<const N: usize> NomEncode for [u8; N] {
    fn encode(&self) -> Vec<u8> {
        self.to_vec()
    }

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        map(take(N), |bytes: &[u8]| {
            let mut arr = [0u8; N];
            arr.copy_from_slice(bytes);
            arr
        })
        .parse(bytes)
    }
}

impl NomEncode for ChannelId {
    fn encode(&self) -> Vec<u8> {
        self.as_ref().to_vec()
    }

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        map(<[u8; 32]>::decode, Self::from).parse(bytes)
    }
}

impl NomEncode for MsgId {
    fn encode(&self) -> Vec<u8> {
        self.as_ref().to_vec()
    }

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        map(<[u8; 32]>::decode, Self::from).parse(bytes)
    }
}

impl NomEncode for Ed25519PublicKey {
    fn encode(&self) -> Vec<u8> {
        self.to_bytes().to_vec()
    }

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        // Ed25519PublicKey = 32BYTE
        map_res(<[u8; _]>::decode, |bytes: [u8; _]| {
            Self::from_bytes(&bytes).map_err(|_| Error::new(bytes, ErrorKind::Fail))
        })
        .parse(bytes)
    }
}
