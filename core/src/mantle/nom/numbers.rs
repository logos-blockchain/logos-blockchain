use lb_groth16::{Fr, fr_from_bytes, fr_to_bytes};
use nom::{
    IResult,
    error::{Error, ErrorKind},
    number::complete::{le_u16, le_u32, le_u64, u8},
};

use crate::mantle::nom::{NomDecode, NomEncode, wire_fixture};

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

// Well-known fixtures for the hand-written primitive codecs. These also satisfy
// the `T: WireExamples` bound that the `BoundedVec<T, ..>` / `[T; N]` blanket
// fixtures rely on.
wire_fixture!(u8, 0x07u8, "07");
wire_fixture!(u16, 0x0201u16, "0102");
wire_fixture!(u32, 0x0403_0201u32, "01020304");
wire_fixture!(u64, 0x0807_0605_0403_0201u64, "0102030405060708");
wire_fixture!(
    Fr,
    Fr::from(1u64),
    "0100000000000000000000000000000000000000000000000000000000000000"
);

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
