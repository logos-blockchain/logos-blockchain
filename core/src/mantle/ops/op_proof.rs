use lb_codec::{BinaryDecode, BinaryEncode, DecodeError};
use lb_key_management_system_keys::keys::{Ed25519Signature, ZkSignature};
use serde::{Deserialize, Serialize};

use crate::proofs::{
    channel_multi_sig_proof::ChannelMultiSigProof, leader_claim_proof::Groth16LeaderClaimProof,
};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZkAndEd25519Proof {
    pub zk_sig: ZkSignature,
    pub ed25519_sig: Ed25519Signature,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpProof {
    Ed25519Sig(Ed25519Signature),
    ZkSig(ZkSignature),
    ZkAndEd25519Sigs(ZkAndEd25519Proof),
    PoC(Groth16LeaderClaimProof),
    ChannelMultiSigProof(ChannelMultiSigProof),
    None(NoOpProof),
}
