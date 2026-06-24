use lb_blend_proofs::{
    quota::{ProofOfQuota, VerifiedProofOfQuota},
    selection::{ProofOfSelection, VerifiedProofOfSelection},
};
use nom::{
    IResult,
    error::{Error, ErrorKind},
};

use crate::mantle::nom::{NomDecode, NomEncode, wire_fixture};

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

wire_fixture!(
    ProofOfQuota,
    VerifiedProofOfQuota::from_bytes_unchecked([1u8; _]).into() => "01010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101"
);

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

wire_fixture!(
    ProofOfSelection,
    VerifiedProofOfSelection::from_bytes_unchecked([1u8; _]).into() => "0101010101010101010101010101010101010101010101010101010101010101"
);
