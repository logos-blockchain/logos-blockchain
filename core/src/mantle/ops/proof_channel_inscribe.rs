use lb_binary_codec::canonical::{BinaryDecode, BinaryEncode, DecodeError};
use lb_key_management_system_keys::keys::{ED25519_SIGNATURE_SIZE, Ed25519Signature};
use serde::{Deserialize, Serialize};

use crate::mantle::ops::{StepProof, StepProofShape, channel::inscribe::InscriptionOp};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChannelInscribeProof {
    // By the signer, over the mantle_txhash.
    pub signature: Ed25519Signature,
    // One per step.
    pub step_proofs: Vec<StepProof>,
}

impl ChannelInscribeProof {
    #[must_use]
    pub const fn new(signature: Ed25519Signature, step_proofs: Vec<StepProof>) -> Self {
        Self {
            signature,
            step_proofs,
        }
    }

    #[must_use]
    pub const fn without_steps(signature: Ed25519Signature) -> Self {
        Self::new(signature, Vec::new())
    }

    /// The shape of the proof of each step of `operation`, the context its
    /// proof decodes with.
    #[must_use]
    pub fn decode_context(operation: &InscriptionOp) -> Vec<StepProofShape> {
        operation.steps().map(StepProofShape::from).collect()
    }

    /// The size of the encoded proof of `operation`.
    #[must_use]
    pub fn encoded_length_for(operation: &InscriptionOp) -> usize {
        ED25519_SIGNATURE_SIZE
            + operation
                .steps()
                .map(|step| StepProofShape::from(step).encoded_length())
                .sum::<usize>()
    }
}

impl BinaryEncode for ChannelInscribeProof {
    fn encoded_length(&self) -> usize {
        ED25519_SIGNATURE_SIZE
            + self
                .step_proofs
                .iter()
                .map(BinaryEncode::encoded_length)
                .sum::<usize>()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.signature.encode_into(out);
        for step_proof in &self.step_proofs {
            step_proof.encode_into(out);
        }
    }
}

impl BinaryDecode for ChannelInscribeProof {
    // The proof shape of each step of the inscription.
    type Context = Vec<StepProofShape>;

    fn decode<'input>(
        input: &'input [u8],
        context: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (mut rest, signature) = Ed25519Signature::decode(input, &())?;
        let mut step_proofs = Vec::with_capacity(context.len());
        for shape in context {
            let (next, step_proof) = StepProof::decode(rest, shape)?;
            step_proofs.push(step_proof);
            rest = next;
        }
        Ok((rest, Self::new(signature, step_proofs)))
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl crate::mantle::ops::op_proof::samples::SampleProof for ChannelInscribeProof {
    fn sample() -> Self {
        Self::without_steps(Ed25519Signature::sample())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mantle::ops::{ZkTransferProof, op_proof::samples::SampleProof as _};

    #[test]
    fn decode_reads_one_zk_transfer_per_step() {
        let proof = ChannelInscribeProof::new(
            Ed25519Signature::from_bytes(&[2u8; 64]),
            vec![
                StepProof::ZkTransfer(ZkTransferProof::sample()),
                StepProof::ZkTransfer(ZkTransferProof::sample()),
            ],
        );
        let bytes = proof.encode();
        assert_eq!(bytes.len(), 64 + 2 * 128);
        assert_eq!(
            ChannelInscribeProof::decode(&bytes, &vec![StepProofShape::ZkTransfer; 2]).unwrap(),
            (&[][..], proof)
        );
        let (rest, without) = ChannelInscribeProof::decode(&bytes, &vec![]).unwrap();
        assert_eq!(
            without,
            ChannelInscribeProof::without_steps(Ed25519Signature::from_bytes(&[2u8; 64]))
        );
        assert_eq!(rest.len(), 2 * 128);
    }
}
