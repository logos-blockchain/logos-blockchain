use lb_binary_codec::canonical::BinaryEncode;
use lb_key_management_system_keys::keys::Ed25519Signature;
use serde::Serialize;

use crate::{
    mantle::{
        OpProof,
        ops::{ChannelInscribeProof, NoOpProof, ZkAndEd25519Proof, ZkTransferProof},
    },
    proofs::channel_multi_sig_proof::ChannelMultiSigProof,
};

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
pub enum OpProofRef<'a> {
    Ed25519Sig(&'a Ed25519Signature),
    ZkTransfer(&'a ZkTransferProof),
    ZkAndEd25519Sigs(&'a ZkAndEd25519Proof),
    ChannelMultiSigProof(&'a ChannelMultiSigProof),
    None(&'a NoOpProof),
    ChannelInscribe(&'a ChannelInscribeProof),
}

impl BinaryEncode for OpProofRef<'_> {
    fn encoded_length(&self) -> usize {
        match self {
            Self::Ed25519Sig(proof) => proof.encoded_length(),
            Self::ZkTransfer(proof) => proof.encoded_length(),
            Self::ZkAndEd25519Sigs(proof) => proof.encoded_length(),
            Self::ChannelMultiSigProof(proof) => proof.encoded_length(),
            Self::None(proof) => proof.encoded_length(),
            Self::ChannelInscribe(proof) => proof.encoded_length(),
        }
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            Self::Ed25519Sig(proof) => proof.encode_into(out),
            Self::ZkTransfer(proof) => proof.encode_into(out),
            Self::ZkAndEd25519Sigs(proof) => proof.encode_into(out),
            Self::ChannelMultiSigProof(proof) => proof.encode_into(out),
            Self::None(proof) => proof.encode_into(out),
            Self::ChannelInscribe(proof) => proof.encode_into(out),
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
        ZkTransferProof => ZkTransfer,
        ZkAndEd25519Proof => ZkAndEd25519Sigs,
        ChannelMultiSigProof => ChannelMultiSigProof,
        NoOpProof => None,
        ChannelInscribeProof => ChannelInscribe,
}
