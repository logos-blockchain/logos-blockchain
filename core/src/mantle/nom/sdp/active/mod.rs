use nom::IResult;

use crate::{
    mantle::{
        nom::{NomDecode, NomEncode},
        ops::sdp::SDPActiveOp,
    },
    sdp::{ActivityMetadata, DeclarationId, Nonce},
};

pub mod blend;

impl NomEncode for SDPActiveOp {
    fn encode(&self) -> Vec<u8> {
        let mut bytes = self.declaration_id.encode();
        bytes.extend(self.nonce.encode());
        bytes.extend(self.metadata.encode());
        bytes
    }
}

impl NomDecode for SDPActiveOp {
    type Output = Self;

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self::Output> {
        let (bytes, declaration_id) = DeclarationId::decode(bytes)?;
        let (bytes, nonce) = Nonce::decode(bytes)?;
        let (bytes, metadata) = ActivityMetadata::decode(bytes)?;

        Ok((
            bytes,
            Self {
                declaration_id,
                nonce,
                metadata,
            },
        ))
    }
}
