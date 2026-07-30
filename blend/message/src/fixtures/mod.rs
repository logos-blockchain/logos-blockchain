use lb_blend_proofs::{
    quota::{VerifiedProofOfQuota, PROOF_OF_QUOTA_SIZE},
    selection::{VerifiedProofOfSelection, PROOF_OF_SELECTION_SIZE},
};
use lb_codec::codec_fixtures;
use lb_key_management_system_keys::keys::{
    Ed25519PublicKey, Ed25519Signature, UnsecuredEd25519Key, ED25519_PUBLIC_KEY_SIZE,
    ED25519_SIGNATURE_SIZE,
};

use crate::{
    encap::{
        encapsulated::{
            EncapsulatedBlendingHeader, EncapsulatedMessage, EncapsulatedPart, EncapsulatedPayload,
            EncapsulatedPrivateHeader,
        },
        validated::{
            EncapsulatedMessageWithVerifiedPublicHeader, EncapsulatedMessageWithVerifiedSignature,
        },
    },
    input::EncapsulationInput,
    message::{
        blending_header::BlendingHeader,
        payload::Payload,
        public_header::{PublicHeader, PublicHeaderWithVerifiedSignature, VerifiedPublicHeader},
    },
    PaddedPayloadBody, PayloadType,
};

// -- Payload ---------------------------------------------------------------

codec_fixtures!(PayloadType, Self::Cover => "00", Self::Data => "01");

codec_fixtures!(
    PaddedPayloadBody,
    Self::try_from(&[1u8, 2, 3][..]).unwrap()
        => include_str!("padded_payload_body.hex")
);

codec_fixtures!(
    Payload,
    Self::new(
        PayloadType::Data,
        PaddedPayloadBody::try_from(&[4u8, 5, 6][..]).unwrap(),
    ) => include_str!("payload.hex")
);

codec_fixtures!(
    EncapsulatedPayload,
    Self::initialize(&Payload::new(
        PayloadType::Data,
        PaddedPayloadBody::try_from(&[7u8, 8, 9][..]).unwrap(),
    )) => include_str!("encapsulated_payload.hex")
);

// -- Headers ---------------------------------------------------------------

codec_fixtures!(
    EncapsulatedPrivateHeader,
    context = core::num::NonZeroU64::new(1).unwrap(),
    Self::try_initialize(
        &[EncapsulationInput::try_new(
            UnsecuredEd25519Key::from_bytes(&[1u8; 32]),
            &UnsecuredEd25519Key::from_bytes(&[2u8; 32]).public_key(),
            VerifiedProofOfQuota::from_bytes_unchecked([0u8; PROOF_OF_QUOTA_SIZE]),
            VerifiedProofOfSelection::from_bytes_unchecked([0u8; PROOF_OF_SELECTION_SIZE]),
        ).unwrap()]
    ).unwrap() => "47a7f32151949c60050ec4454b43fcaf351a2f2383ddef2a6ab4176e269e34477821d1abaf629a228ad07b998628f4dd1e137827ca30ec8d99e90aaf9ff355af72e911fbe5eaaf7a867ca80e0a45d5a00c89a7360996aaf496503291d771adeb9caed0ca2bc20af7c31ecea182b4eb797300b68a4e5001ee438e45b402993984782478001f7336041173182189484d18804b75fb1b753c8c7cc0ae56d45c1d5b281ed36752418b833ac7e8d97bb2f78a3ac0ef9704c4f4c61ebd1c2bbfb3806dabbd2ef7b33c7778ce23a4133ac0dcf3d39c43f0562090f590506fd30e38eae7b8eb89690481bbb9a9848921d9d951b56a4ad15eec0093997cf07c04722b32edccf3bec96815f21a40d1e40e7fe5cea75d821f9763339402a92e136541b6837c7e"
);

codec_fixtures!(
    EncapsulatedBlendingHeader,
    Self::initialize(&BlendingHeader::pseudo_random(&[1u8; 32])) => "9c05c033b59c05091fe7e3bd1bbc4abc77f69a71ba282f10e6675bb051fd4a2a5875f96f219d2a8fadfd1ed5af6476c35d6be6a485647abf2709569a69b0aa1c030ca0e01eed758d1158e36e487c1282ec22aa61edbfdd7032067707f477951f27e8f5e76ef941dde3de62a3cea5a9b6ed3e5294a836c6cb6b50eda8a09a27f57fe0e30ffcc8b93c159708ac9a230eda377da92d675c8db8e95c2f9009e1ae63e10278547404d7b335a4a749f2811570e34d6588123e1c2a614faabd0602a3fb4698466e50305eafcddebcd175c63bd560e47c883a993c87a9d9460db9ca83c56602f1eee75124c22637baa8d9fc42a62b3a34317dd4ab53e3d71442dadec7b1e8a4c258d208a3d3662a4f83674f93d6074a81e902c33d4adcbd1c995f85281200"
);

codec_fixtures!(
    BlendingHeader,
    Self {
        signing_pubkey: Ed25519PublicKey::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE]).unwrap(),
        proof_of_quota: VerifiedProofOfQuota::from_bytes_unchecked([1; PROOF_OF_QUOTA_SIZE])
            .into_inner(),
        signature: Ed25519Signature::from_bytes(&[2; ED25519_SIGNATURE_SIZE]),
        proof_of_selection: VerifiedProofOfSelection::from_bytes_unchecked(
            [3; PROOF_OF_SELECTION_SIZE],
        )
        .into_inner(),
        is_last: false,
    } => "00000000000000000000000000000000000000000000000000000000000000000101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010102020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202030303030303030303030303030303030303030303030303030303030303030300"
);

/// The well-known bytes of a `PublicHeader` (version `0x01`, the reconstructed
/// signing key of all `0x00`, a proof of quota of all `0x01`, and a signature
/// of all `0x02`). Shared by the `PublicHeader` fixture and the two verified
/// wrappers, which encode to the same bytes.
const PUBLIC_HEADER_HEX: &str = "0100000000000000000000000000000000000000000000000000000000000000000101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010102020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202";

codec_fixtures!(
    PublicHeader,
    Self::new(
        Ed25519PublicKey::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE]).unwrap(),
        &VerifiedProofOfQuota::from_bytes_unchecked([1; PROOF_OF_QUOTA_SIZE]).into_inner(),
        Ed25519Signature::from_bytes(&[2; ED25519_SIGNATURE_SIZE]),
    ) => PUBLIC_HEADER_HEX
);

codec_fixtures!(
    PublicHeaderWithVerifiedSignature,
    encode_only,
    Self::new(
        VerifiedProofOfQuota::from_bytes_unchecked([1; PROOF_OF_QUOTA_SIZE]).into_inner(),
        Ed25519PublicKey::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE]).unwrap(),
        Ed25519Signature::from_bytes(&[2; ED25519_SIGNATURE_SIZE]),
    ) => PUBLIC_HEADER_HEX
);

codec_fixtures!(
    VerifiedPublicHeader,
    encode_only,
    Self::new(
        VerifiedProofOfQuota::from_bytes_unchecked([1; PROOF_OF_QUOTA_SIZE]),
        Ed25519PublicKey::from_bytes(&[0; ED25519_PUBLIC_KEY_SIZE]).unwrap(),
        Ed25519Signature::from_bytes(&[2; ED25519_SIGNATURE_SIZE]),
    ) => PUBLIC_HEADER_HEX
);

// -- Encapsulated message --------------------------------------------------
//
// All three message types encode to the same bytes: a genuine, deterministic
// single-layer encapsulation built by [`wire_fixture_message`].

fn wire_fixture_message() -> EncapsulatedMessageWithVerifiedPublicHeader {
    let recipient_signing_key = UnsecuredEd25519Key::from_bytes(&[1u8; 32]);
    let inputs = [EncapsulationInput::try_new(
        UnsecuredEd25519Key::from_bytes(&[2u8; 32]),
        &recipient_signing_key.public_key(),
        VerifiedProofOfQuota::from_bytes_unchecked([0u8; PROOF_OF_QUOTA_SIZE]),
        VerifiedProofOfSelection::from_bytes_unchecked([0u8; PROOF_OF_SELECTION_SIZE]),
    )
    .expect("well-known encapsulation input is valid")];

    let payload_body = PaddedPayloadBody::try_from(b"well-known blend message payload".as_ref())
        .expect("payload body fits");

    let (part, signing_key, proof_of_quota) = inputs.iter().enumerate().fold(
        (
            EncapsulatedPart::try_initialize(&inputs, PayloadType::Data, payload_body)
                .expect("inputs are non-empty"),
            // Fixed stand-ins for `try_new`'s randomly-sampled outer-sender identity.
            UnsecuredEd25519Key::from_bytes(&[3u8; 32]),
            VerifiedProofOfQuota::from_bytes_unchecked([0u8; PROOF_OF_QUOTA_SIZE]),
        ),
        |(part, signing_key, proof_of_quota), (i, input)| {
            (
                part.encapsulate(
                    input.ephemeral_encryption_key(),
                    &signing_key,
                    &proof_of_quota,
                    *input.proof_of_selection(),
                    i == 0,
                ),
                input.ephemeral_signing_key().clone(),
                *input.proof_of_quota(),
            )
        },
    );

    EncapsulatedMessageWithVerifiedPublicHeader::from_components(
        VerifiedPublicHeader::new(
            proof_of_quota,
            signing_key.public_key(),
            part.sign(&signing_key),
        ),
        part,
    )
}

codec_fixtures!(
    EncapsulatedMessage,
    decode_only,
    context = core::num::NonZeroU64::new(1).unwrap(),
    EncapsulatedMessage::from(wire_fixture_message())
        => include_str!("encapsulated_message.hex")
);

codec_fixtures!(
    EncapsulatedPart,
    context = core::num::NonZeroU64::new(1).unwrap(),
    EncapsulatedMessage::from(wire_fixture_message())
        .into_components()
        .1 => include_str!("encapsulated_part.hex")
);

codec_fixtures!(
    EncapsulatedMessageWithVerifiedSignature,
    encode_only,
    EncapsulatedMessageWithVerifiedSignature::from(wire_fixture_message())
        => include_str!("encapsulated_message.hex")
);

codec_fixtures!(
    EncapsulatedMessageWithVerifiedPublicHeader,
    encode_only,
    wire_fixture_message() => include_str!("encapsulated_message.hex")
);
