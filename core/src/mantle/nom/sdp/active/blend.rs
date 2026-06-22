use lb_blend_proofs::{
    quota::{PROOF_OF_QUOTA_SIZE, ProofOfQuota},
    selection::{PROOF_OF_SELECTION_SIZE, ProofOfSelection},
};
use nom::{
    IResult,
    error::{Error, ErrorKind},
};

use crate::mantle::nom::{NomArray, NomDecode, NomEncode};

type ProofOfQuotaNom<'a> = NomArray<'a, u8, PROOF_OF_QUOTA_SIZE>;

impl NomEncode for ProofOfQuota {
    fn encode(&self) -> Vec<u8> {
        let proof_bytes = <[u8; PROOF_OF_QUOTA_SIZE]>::from(self);
        ProofOfQuotaNom::from(&proof_bytes).encode()
    }
}

impl NomDecode for ProofOfQuota {
    type Output = Self;

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self::Output> {
        let (remaining_bytes, value) = ProofOfQuotaNom::decode(bytes)?;
        Ok((
            remaining_bytes,
            Self::try_from(value)
                .map_err(|_| nom::Err::Error(Error::new(bytes, ErrorKind::MapRes)))?,
        ))
    }
}

type ProofOfSelectionNom<'a> = NomArray<'a, u8, PROOF_OF_SELECTION_SIZE>;

impl NomEncode for ProofOfSelection {
    fn encode(&self) -> Vec<u8> {
        let proof_bytes = <[u8; PROOF_OF_SELECTION_SIZE]>::from(self);
        ProofOfSelectionNom::from(&proof_bytes).encode()
    }
}

impl NomDecode for ProofOfSelection {
    type Output = Self;

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self::Output> {
        let (remaining_bytes, value) = ProofOfSelectionNom::decode(bytes)?;
        Ok((
            remaining_bytes,
            Self::try_from(value)
                .map_err(|_| nom::Err::Error(Error::new(bytes, ErrorKind::MapRes)))?,
        ))
    }
}
