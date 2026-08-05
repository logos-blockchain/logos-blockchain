use lb_blend_proofs::{
    quota::{PROOF_OF_QUOTA_SIZE, VerifiedProofOfQuota},
    selection::{PROOF_OF_SELECTION_SIZE, VerifiedProofOfSelection},
};
use lb_codec::codec_fixtures;
use lb_key_management_system_keys::keys::{
    ED25519_PUBLIC_KEY_SIZE, ED25519_SIGNATURE_SIZE, Ed25519PublicKey, Ed25519Signature,
    UnsecuredEd25519Key,
};

use crate::{
    PaddedPayloadBody, PayloadType,
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
        payload::{MAX_PAYLOAD_BODY_SIZE, Payload},
        public_header::{PublicHeader, PublicHeaderWithVerifiedSignature, VerifiedPublicHeader},
    },
};

// -- Payload ---------------------------------------------------------------

/// Build a fixture body of exactly [`MAX_PAYLOAD_BODY_SIZE`] bytes: `prefix`,
/// then `0xAA` to the end.
///
/// A shorter body would be padded out with random bytes, which no golden
/// fixture can pin. One that already fills the buffer leaves nothing to pad, so
/// the encoding is deterministic.
fn full_length_body(prefix: &[u8]) -> PaddedPayloadBody {
    let mut body = prefix.to_vec();
    body.resize(MAX_PAYLOAD_BODY_SIZE, 0xAA);
    PaddedPayloadBody::try_from(body).expect("body is exactly the maximum size")
}

codec_fixtures!(PayloadType, Self::Cover => "00", Self::Data => "01");

codec_fixtures!(
    PaddedPayloadBody,
    full_length_body(&[1u8, 2, 3]) => include_str!("padded_payload_body.hex")
);

codec_fixtures!(
    Payload,
    Self::new(
        PayloadType::Data,
        full_length_body(&[4u8, 5, 6]),
    ) => include_str!("payload.hex")
);

codec_fixtures!(
    EncapsulatedPayload,
    Self::initialize(&Payload::new(
        PayloadType::Data,
        full_length_body(&[7u8, 8, 9]),
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
        ).unwrap()], 1
    ).unwrap() => "47a7f32151949c60050ec4454b43fcaf351a2f2383ddef2a6ab4176e269e34477821d1abaf629a228ad07b998628f4dd1e137827ca30ec8d99e90aaf9ff355af71e9112b29ca09a6d2ce84e3e08d2828e473dbb12d47a59d13719d7d3e067a5d9caed0ca2bc20af7c31ecea182b4eb797300b68a4e5001ee438e45b402993984782478001f7336041173182189484d18804b75fb1b753c8c7cc0ae56d45c1d5b281ed36752418b833ac7e8d97bb2f78a3ac0ef9704c4f4c61ebd1c2bbfb3806ddd34430312a0083ae5e49cabf8d9bec8ec93a54138ee10b4997fbaed8fc2ce5223614261e3234df6bfde5fde4632f91f9fad2c9e0f6fa6b8ab0b682328f1d6104765095b9ef782e45805b133ee6156d140ffde4711336612edd80501c9afe407f5018607ed6de1d5e2d42df4e0acf6c2cda901b79a65df6ecd1dc25df2d39af8d4"
);

codec_fixtures!(
    EncapsulatedBlendingHeader,
    Self::initialize(&BlendingHeader::pseudo_random(&[1u8; 32])) => "9c05c033b59c05091fe7e3bd1bbc4abc77f69a71ba282f10e6675bb051fd4a2a5875f96f219d2a8fadfd1ed5af6476c35d6be6a485647abf2709569a69b0aa1c030ca0e01eed758d1158e36e487c1282ec22aa61edbfdd7032067707f477951f27e8f5e76ef941dde3de62a3cea5a9b6ed3e5294a836c6cb6b50eda8a09a27f57fe0e30ffcc8b93c159708ac9a230eda377da92d675c8db8e95c2f9009e1ae63e10278547404d7b335a4a749f2811570e34d6588123e1c2a614faabd0602a3fb72ee85e99628a159c065d228cee1de0ab141a40369de68b0031514a346cc7d184698466e50305eafcddebcd175c63bd560e47c883a993c87a9d9460db9ca83c56602f1eee75124c22637baa8d9fc42a62b3a34317dd4ab53e3d71442dadec7b1e8a4c258d208a3d3662a4f83674f93d6074a81e902c33d4adcbd1c995f85281200"
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
    } => "000000000000000000000000000000000000000000000000000000000000000001010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010102020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202030303030303030303030303030303030303030303030303030303030303030300"
);

/// The well-known bytes of a `PublicHeader` (version `0x01`, the reconstructed
/// signing key of all `0x00`, a proof of quota of all `0x01`, and a signature
/// of all `0x02`). Shared by the `PublicHeader` fixture and the two verified
/// wrappers, which encode to the same bytes.
const PUBLIC_HEADER_HEX: &str = "01000000000000000000000000000000000000000000000000000000000000000001010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010102020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202";

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
// All three message types encode to the same bytes: a genuine, deterministi
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

    let payload_body = full_length_body(b"well-known blend message payload");

    let (part, signing_key, proof_of_quota) = inputs.iter().enumerate().fold(
        (
            EncapsulatedPart::try_initialize(
                &inputs,
                PayloadType::Data,
                payload_body,
                inputs.len(),
            )
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
