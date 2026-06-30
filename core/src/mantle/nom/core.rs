use lb_cryptarchia_engine::Epoch;
use nom::IResult;

use crate::mantle::nom::{NomDecode, NomEncode};

impl NomEncode for Epoch {
    fn encode(&self) -> Vec<u8> {
        self.as_ref().encode()
    }
}

impl NomDecode for Epoch {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        let (bytes, inner) = u32::decode(bytes)?;
        Ok((bytes, Self::new(inner)))
    }
}
