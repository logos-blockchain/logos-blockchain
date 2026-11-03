use lb_binary_codec::canonical::codec_fixtures;

use super::proof_values::{
    CHANNEL_INSCRIBE_PROOF, CHANNEL_INSCRIBE_PROOF_HEX, CHANNEL_MULTI_SIG, CHANNEL_MULTI_SIG_HEX,
    ED25519_SIG, ED25519_SIG_HEX, ZK_AND_ED25519_SIGS, ZK_AND_ED25519_SIGS_HEX, ZK_TRANSFER,
    ZK_TRANSFER_HEX,
};
use crate::mantle::ops::{NoOpProof, OpProofRef};

codec_fixtures!(
    OpProofRef<'_>,
    encode_only,
    OpProofRef::None(&NoOpProof) => "",
    OpProofRef::ZkTransfer(&ZK_TRANSFER) => ZK_TRANSFER_HEX,
    OpProofRef::Ed25519Sig(&ED25519_SIG) => ED25519_SIG_HEX,
    OpProofRef::ZkAndEd25519Sigs(&ZK_AND_ED25519_SIGS) => ZK_AND_ED25519_SIGS_HEX,
    OpProofRef::ChannelMultiSigProof(&CHANNEL_MULTI_SIG) => CHANNEL_MULTI_SIG_HEX,
    OpProofRef::ChannelInscribe(&CHANNEL_INSCRIBE_PROOF) => CHANNEL_INSCRIBE_PROOF_HEX
);
