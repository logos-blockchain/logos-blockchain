use lb_blend_proofs::{
    quota::{PROOF_OF_QUOTA_SIZE, ProofOfQuota},
    selection::{PROOF_OF_SELECTION_SIZE, ProofOfSelection},
};
use nom::{IResult, Parser as _, combinator::map_res};

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
        map_res(ProofOfQuotaNom::decode, Self::try_from).parse(bytes)
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
        map_res(ProofOfSelectionNom::decode, Self::try_from).parse(bytes)
    }
}
