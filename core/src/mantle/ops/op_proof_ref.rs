use lb_codec::BinaryEncode;
use lb_key_management_system_keys::keys::{Ed25519Signature, ZkSignature};
use serde::Serialize;

use crate::{
    mantle::{
        OpProof,
        ops::{NoOpProof, ZkAndEd25519Proof},
    },
    proofs::{
        channel_multi_sig_proof::ChannelMultiSigProof, leader_claim_proof::Groth16LeaderClaimProof,
    },
};

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
pub enum OpProofRef<'a> {
    Ed25519Sig(&'a Ed25519Signature),
    ZkSig(&'a ZkSignature),
    ZkAndEd25519Sigs(&'a ZkAndEd25519Proof),
    PoC(&'a Groth16LeaderClaimProof),
    ChannelMultiSigProof(&'a ChannelMultiSigProof),
    None(&'a NoOpProof),
}

impl BinaryEncode for OpProofRef<'_> {
    fn encoded_length(&self) -> usize {
        match self {
            Self::Ed25519Sig(proof) => proof.encoded_length(),
            Self::ZkSig(proof) => proof.encoded_length(),
            Self::ZkAndEd25519Sigs(proof) => proof.encoded_length(),
            Self::PoC(proof) => proof.encoded_length(),
            Self::ChannelMultiSigProof(proof) => proof.encoded_length(),
            Self::None(proof) => proof.encoded_length(),
        }
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            Self::Ed25519Sig(proof) => proof.encode_into(out),
            Self::ZkSig(proof) => proof.encode_into(out),
            Self::ZkAndEd25519Sigs(proof) => proof.encode_into(out),
            Self::PoC(proof) => proof.encode_into(out),
            Self::ChannelMultiSigProof(proof) => proof.encode_into(out),
            Self::None(proof) => proof.encode_into(out),
        }
    }
}

macro_rules! impl_owned_ref_conversions {
    ($enum:ident => $($ty:ty => $variant:ident),* $(,)?) => {
        impl<'a> From<&'a $enum> for OpProofRef<'a> {
            fn from(proof: &'a $enum) -> Self {
                match proof { $($enum::$variant(proof) => Self::$variant(proof)),* }
            }
        }
        $(
            impl<'a> From<&'a $ty> for OpProofRef<'a> {
                fn from(proof: &'a $ty) -> Self { Self::$variant(proof) }
            }
        )*
    };
}

impl_owned_ref_conversions! {
    OpProof =>
        Ed25519Signature => Ed25519Sig,
        ZkSignature => ZkSig,
        ZkAndEd25519Proof => ZkAndEd25519Sigs,
        Groth16LeaderClaimProof => PoC,
        ChannelMultiSigProof => ChannelMultiSigProof,
        NoOpProof => None,
}
