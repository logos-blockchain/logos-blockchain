use nom::IResult;

use crate::{
    mantle::{
        NoteId,
        nom::{NomDecode, NomEncode},
        ops::sdp::SDPWithdrawOp,
    },
    sdp::DeclarationId,
};

impl NomEncode for SDPWithdrawOp {
    fn encode(&self) -> Vec<u8> {
        let mut bytes = self.declaration_id.encode();
        bytes.extend(self.nonce.encode());
        bytes.extend(self.locked_note_id.encode());
        bytes
    }
}

impl NomDecode for SDPWithdrawOp {
    type Output = Self;

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self::Output> {
        let (bytes, declaration_id) = DeclarationId::decode(bytes)?;
        let (bytes, nonce) = u64::decode(bytes)?;
        let (bytes, locked_note_id) = NoteId::decode(bytes)?;

        Ok((
            bytes,
            Self {
                declaration_id,
                locked_note_id,
                nonce,
            },
        ))
    }
}
