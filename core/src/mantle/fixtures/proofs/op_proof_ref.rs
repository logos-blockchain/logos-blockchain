use lb_codec::codec_fixtures;

use super::proof_values::{
    CHANNEL_MULTI_SIG, CHANNEL_MULTI_SIG_HEX, ED25519_SIG, ED25519_SIG_HEX, POC, POC_HEX,
    ZK_AND_ED25519_SIGS, ZK_AND_ED25519_SIGS_HEX, ZK_SIG, ZK_SIG_HEX,
};
use crate::mantle::ops::{NoOpProof, OpProofRef};

codec_fixtures!(
    OpProofRef<'_>,
    encode_only,
    OpProofRef::None(&NoOpProof) => "",
    OpProofRef::ZkSig(&ZK_SIG) => ZK_SIG_HEX,
    OpProofRef::Ed25519Sig(&ED25519_SIG) => ED25519_SIG_HEX,
    OpProofRef::ZkAndEd25519Sigs(&ZK_AND_ED25519_SIGS) => ZK_AND_ED25519_SIGS_HEX,
    OpProofRef::PoC(&POC) => POC_HEX,
    OpProofRef::ChannelMultiSigProof(&CHANNEL_MULTI_SIG) => CHANNEL_MULTI_SIG_HEX
);
