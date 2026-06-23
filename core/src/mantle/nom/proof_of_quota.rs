use lb_blend_proofs::{quota::ProofOfQuota, selection::ProofOfSelection};
use nom::{
    IResult,
    error::{Error, ErrorKind},
};

use crate::mantle::nom::{NomDecode, NomEncode};

impl NomEncode for ProofOfQuota {
    fn encode(&self) -> Vec<u8> {
        <[u8; _]>::from(self).encode()
    }
}

impl NomDecode for ProofOfQuota {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        let (remaining_bytes, value) = <[u8; _]>::decode(bytes)?;
        Ok((
            remaining_bytes,
            Self::try_from(value)
                .map_err(|_| nom::Err::Error(Error::new(bytes, ErrorKind::MapRes)))?,
        ))
    }
}

impl NomEncode for ProofOfSelection {
    fn encode(&self) -> Vec<u8> {
        <[u8; _]>::from(self).encode()
    }
}

impl NomDecode for ProofOfSelection {
    fn decode(bytes: &[u8]) -> IResult<&[u8], Self> {
        let (remaining_bytes, value) = <[u8; _]>::decode(bytes)?;
        Ok((
            remaining_bytes,
            Self::try_from(value)
                .map_err(|_| nom::Err::Error(Error::new(bytes, ErrorKind::MapRes)))?,
        ))
    }
}
