use lb_binary_codec::canonical::codec_fixtures;
use lb_blend_proofs::{quota::VerifiedProofOfQuota, selection::VerifiedProofOfSelection};
use lb_cryptarchia_engine::Epoch;
use lb_groth16::Fr;
use lb_key_management_system_keys::keys::{Ed25519PublicKey, ZkPublicKey};

use crate::{
    mantle::fixtures::ops::op_values::{
        SDP_ACTIVE, SDP_ACTIVE_PAYLOAD_HEX, SDP_DECLARE, SDP_DECLARE_PAYLOAD_HEX, SDP_WITHDRAW,
        SDP_WITHDRAW_PAYLOAD_HEX,
    },
    sdp::{
        ActiveMessage, ActivityMetadata, DeclarationId, DeclarationMessage, Locator, Nonce,
        ProviderId, ServiceType, WithdrawMessage, blend::ActivityProof,
    },
};

codec_fixtures!(
    Locator,
    Self::new_unchecked("/ip4/127.0.0.1/udp/3000/quic-v1".parse().unwrap()) => "0b00047f00000191020bb8cd03"
);
codec_fixtures!(ServiceType, Self::BlendNetwork => "00");
codec_fixtures!(ProviderId, Self(Ed25519PublicKey::from_bytes(&[1u8; _]).unwrap()) => "0101010101010101010101010101010101010101010101010101010101010101");
codec_fixtures!(DeclarationId, Self([0u8; _]) => "0000000000000000000000000000000000000000000000000000000000000000");
codec_fixtures!(
    Nonce,
    Self::new(Epoch::new(0), 0x1d) => "1d00000000000000",
    Self::new(Epoch::new(0x0102_0304), 0x0506_0708) => "0807060504030201",
);
codec_fixtures!(
    DeclarationMessage,
    Self {
        service_type: ServiceType::BlendNetwork,
        locators: [Locator::new_unchecked("/ip4/127.0.0.1/udp/3000/quic-v1".parse().unwrap())].into(),
        provider_id: ProviderId(Ed25519PublicKey::from_bytes(&[1u8; _]).unwrap()),
        zk_id: ZkPublicKey::new(Fr::from(1u64)),
        service_note_id: Fr::from(0u64).into(),
    } => "00010b00047f00000191020bb8cd03010101010101010101010101010101010101010101010101010101010101010101000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    SDP_DECLARE.clone() => SDP_DECLARE_PAYLOAD_HEX,
);
codec_fixtures!(
    WithdrawMessage,
    Self {
        declaration_id: DeclarationId([0u8; _]),
        nonce: Nonce::new(Epoch::new(0), 2)
    } => "00000000000000000000000000000000000000000000000000000000000000000200000000000000",
    *SDP_WITHDRAW => SDP_WITHDRAW_PAYLOAD_HEX,
);
codec_fixtures!(
    ActiveMessage,
    Self {
        declaration_id: DeclarationId([0u8; _]),
        nonce: Nonce::new(Epoch::new(0), 0),
        metadata: ActivityMetadata::Blend(Box::new(ActivityProof {
            epoch: Epoch::new(10),
            signing_key: Ed25519PublicKey::from_bytes(&[1u8; _]).unwrap(),
            proof_of_quota:
                VerifiedProofOfQuota::from_bytes_unchecked([0u8; _]).into(),
            proof_of_selection:
                VerifiedProofOfSelection::from_bytes_unchecked([1u8; _])
                    .into(),
        })),
    } => "0000000000000000000000000000000000000000000000000000000000000000000000000000000001010a0000000101010101010101010101010101010101010101010101010101010101010101000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000101010101010101010101010101010101010101010101010101010101010101",
    SDP_ACTIVE.clone() => SDP_ACTIVE_PAYLOAD_HEX,
);
codec_fixtures!(
    ActivityProof,
    Self {
        epoch: Epoch::new(10),
        signing_key: Ed25519PublicKey::from_bytes(&[1u8; _]).unwrap(),
        proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked(
            [0u8; _]
        )
        .into(),
        proof_of_selection:
            VerifiedProofOfSelection::from_bytes_unchecked([1u8; _])
                .into(),
    } => "010a0000000101010101010101010101010101010101010101010101010101010101010101000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000101010101010101010101010101010101010101010101010101010101010101"
);
codec_fixtures!(
    ActivityMetadata,
    Self::Blend(Box::new(ActivityProof {
        epoch: Epoch::new(10),
        signing_key: Ed25519PublicKey::from_bytes(&[1u8; _]).unwrap(),
        proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked(
            [0u8; _]
        )
        .into(),
        proof_of_selection:
            VerifiedProofOfSelection::from_bytes_unchecked([1u8; _])
                .into(),
    })) => "01010a0000000101010101010101010101010101010101010101010101010101010101010101000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000101010101010101010101010101010101010101010101010101010101010101"
);
