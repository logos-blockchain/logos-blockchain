use lb_groth16::{Fr, fr_from_bytes, fr_to_bytes};
use nom::{
    IResult,
    error::{Error, ErrorKind},
    number::complete::{le_u16, le_u32, le_u64, u8},
};

use crate::mantle::nom::{NomDecode, NomEncode};

impl NomEncode for u8 {
    fn encode(&self) -> Vec<u8> {
        vec![*self]
    }
}

impl NomDecode for u8 {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        u8(bytes)
    }
}

impl NomEncode for u16 {
    fn encode(&self) -> Vec<u8> {
        self.to_le_bytes().to_vec()
    }
}

impl NomDecode for u16 {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        le_u16(bytes)
    }
}

impl NomEncode for u32 {
    fn encode(&self) -> Vec<u8> {
        self.to_le_bytes().to_vec()
    }
}

impl NomDecode for u32 {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        le_u32(bytes)
    }
}

impl NomEncode for u64 {
    fn encode(&self) -> Vec<u8> {
        self.to_le_bytes().to_vec()
    }
}

impl NomDecode for u64 {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        le_u64(bytes)
    }
}

impl NomEncode for Fr {
    fn encode(&self) -> Vec<u8> {
        fr_to_bytes(self).encode()
    }
}

impl NomDecode for Fr {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        let (remaining_bytes, inner) = <[u8; 32]>::decode(bytes)?;
        Ok((
            remaining_bytes,
            fr_from_bytes(&inner)
                .map_err(|_| nom::Err::Error(Error::new(bytes, ErrorKind::MapRes)))?,
        ))
    }
}
