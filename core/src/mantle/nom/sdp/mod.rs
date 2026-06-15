use nom::{IResult, Parser as _, combinator::map};

use crate::{
    mantle::nom::{NomArray, NomDecode, NomEncode},
    sdp::DeclarationId,
};

pub mod declare;
pub mod withdraw;

impl NomEncode for DeclarationId {
    fn encode(&self) -> Vec<u8> {
        NomArray::<u8, 32>::from(&self.0).encode()
    }
}

impl NomDecode for DeclarationId {
    type Output = Self;

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self::Output> {
        map(NomArray::<u8, 32>::decode, Self).parse(bytes)
    }
}
