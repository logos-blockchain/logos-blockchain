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

impl<T> NomEncode for [T]
where
    T: NomEncode,
{
    fn encode(&self) -> Vec<u8> {
        let mut bytes = (self.len() as u8).encode();
        for item in self {
            bytes.extend(item.encode());
        }
        bytes
    }
}

impl<T, const N: usize> NomEncode for BoundedVec<T, N>
where
    T: NomEncode,
{
    fn encode(&self) -> Vec<u8> {
        self.as_slice().encode()
    }

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        let (input, op_count) = u8::decode(bytes)?;

        if op_count as usize > N {
            return Err(nom::Err::Error(Error::new(input, ErrorKind::TooLarge)));
        }

        let (input, items) = count(T::decode, op_count as usize).parse(input)?;

        Ok((input, Self::new_unchecked(items)))
    }
}

impl<T> NomEncode for Vec<T>
where
    T: NomEncode,
{
    fn encode(&self) -> Vec<u8> {
        self.as_slice().encode()
    }

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        let (input, op_count) = u8::decode(bytes)?;

        count(T::decode, op_count as usize).parse(input)
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
