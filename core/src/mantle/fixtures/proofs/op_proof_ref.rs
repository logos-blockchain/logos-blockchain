use std::sync::LazyLock;

use lb_codec::codec_fixtures;
use lb_groth16::CompressedGroth16Proof;
use lb_key_management_system_keys::keys::{Ed25519Signature, ZkSignature};

use crate::mantle::ops::{NoOpProof, OpProofRef};

static ZK_SIG: LazyLock<ZkSignature> =
    LazyLock::new(|| ZkSignature::new(CompressedGroth16Proof::from_bytes(&[1u8; 128])));

static ED25519_SIG: LazyLock<Ed25519Signature> =
    LazyLock::new(|| Ed25519Signature::from_bytes(&[1u8; 64]));

codec_fixtures!(
    OpProofRef<'_>,
    encode_only,
    OpProofRef::None(&NoOpProof) => "",
    OpProofRef::ZkSig(&ZK_SIG) => "0101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101",
    OpProofRef::Ed25519Sig(&ED25519_SIG) => "01010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101"
);
