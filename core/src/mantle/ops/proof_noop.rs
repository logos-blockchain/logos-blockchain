use lb_codec::{BinaryDecode, BinaryEncode, DecodeError};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoOpProof;

impl BinaryEncode for NoOpProof {
    fn encoded_length(&self) -> usize {
        0
    }

    fn encode_into(&self, _out: &mut Vec<u8>) {}
}

impl BinaryDecode for NoOpProof {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        _context: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        Ok((input, Self))
    }
}

#[cfg(any(test, feature = "samples"))]
impl crate::mantle::ops::op_proof::samples::SampleProof for NoOpProof {
    fn sample() -> Self {
        Self
    }
}
