use lb_blend_proofs::{quota::VerifiedProofOfQuota, selection::VerifiedProofOfSelection};
use lb_core_macros::nom_wire_fixtures;
use lb_cryptarchia_engine::Epoch;
use lb_groth16::Fr;
use lb_key_management_system_keys::keys::{Ed25519PublicKey, ZkPublicKey};

use crate::sdp::{
    ActiveMessage, ActivityMetadata, DeclarationId, DeclarationMessage, Locator, ProviderId,
    ServiceType, WithdrawMessage, blend::ActivityProof,
};

nom_wire_fixtures!(
    Locator,
    Self::new_unchecked("/ip4/127.0.0.1/udp/3000/quic-v1".parse().unwrap()) => "0b00047f00000191020bb8cd03"
);
nom_wire_fixtures!(ServiceType, Self::BlendNetwork => "00");
nom_wire_fixtures!(ProviderId, Self(Ed25519PublicKey::from_bytes(&[0u8; _]).unwrap()) => "0000000000000000000000000000000000000000000000000000000000000000");
nom_wire_fixtures!(DeclarationId, Self([0u8; _]) => "0000000000000000000000000000000000000000000000000000000000000000");
nom_wire_fixtures!(
    DeclarationMessage,
    Self {
        service_type: ServiceType::BlendNetwork,
        locators: [Locator::new_unchecked("/ip4/127.0.0.1/udp/3000/quic-v1".parse().unwrap())].into(),
        provider_id: ProviderId(Ed25519PublicKey::from_bytes(&[0u8; _]).unwrap()),
        zk_id: ZkPublicKey::new(Fr::from(1u64)),
        locked_note_id: Fr::from(0u64).into(),
    } => "00010b00047f00000191020bb8cd03000000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
);
nom_wire_fixtures!(
    WithdrawMessage,
    Self {
        declaration_id: DeclarationId([0u8; _]),
        locked_note_id: Fr::from(1u64).into(),
        nonce: 2u64
    } => "000000000000000000000000000000000000000000000000000000000000000002000000000000000100000000000000000000000000000000000000000000000000000000000000",
);
nom_wire_fixtures!(
    ActiveMessage,
    Self {
        declaration_id: DeclarationId([0u8; _]),
        nonce: 0u64,
        metadata: ActivityMetadata::Blend(Box::new(ActivityProof {
            epoch: Epoch::new(10),
            signing_key: Ed25519PublicKey::from_bytes(&[0u8; _]).unwrap(),
            proof_of_quota:
                VerifiedProofOfQuota::from_bytes_unchecked([0u8; _]).into(),
            proof_of_selection:
                VerifiedProofOfSelection::from_bytes_unchecked([1u8; _])
                    .into(),
        })),
    } => "0000000000000000000000000000000000000000000000000000000000000000000000000000000001010a0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000101010101010101010101010101010101010101010101010101010101010101"
);
nom_wire_fixtures!(
    ActivityProof,
    Self {
        epoch: Epoch::new(10),
        signing_key: Ed25519PublicKey::from_bytes(&[0u8; _]).unwrap(),
        proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked(
            [0u8; _]
        )
        .into(),
        proof_of_selection:
            VerifiedProofOfSelection::from_bytes_unchecked([1u8; _])
                .into(),
    } => "010a0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000101010101010101010101010101010101010101010101010101010101010101"
);
nom_wire_fixtures!(
    ActivityMetadata,
    Self::Blend(Box::new(ActivityProof {
        epoch: Epoch::new(10),
        signing_key: Ed25519PublicKey::from_bytes(&[0u8; _]).unwrap(),
        proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked(
            [0u8; _]
        )
        .into(),
        proof_of_selection:
            VerifiedProofOfSelection::from_bytes_unchecked([1u8; _])
                .into(),
    })) => "01010a0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000101010101010101010101010101010101010101010101010101010101010101"
);
