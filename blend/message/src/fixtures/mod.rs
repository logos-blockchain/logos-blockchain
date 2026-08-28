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

codec_fixtures!(
    PayloadType,
    Self::Cover => "00",
    Self::BlockProposal => "01",
    Self::Transaction => "02"
);

codec_fixtures!(
    PaddedPayloadBody,
    full_length_body(&[1u8, 2, 3]) => include_str!("padded_payload_body.hex")
);

codec_fixtures!(
    Payload,
    Self::new(
        PayloadType::BlockProposal,
        full_length_body(&[4u8, 5, 6]),
    ) => include_str!("payload.hex")
);

codec_fixtures!(
    EncapsulatedPayload,
    Self::initialize(&Payload::new(
        PayloadType::BlockProposal,
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
    ).unwrap() => "aac741e4d4aa7dda96f8f12343513612b5b4aaef97b76eeabccd84305ea75366cfa700c71fc60e212e0a81aeadecf66902674a92cf5b9bb8737cc4cda97dd7c4c8a32ce737a704492afbfa0e5e9988d1e746708b59e5aa250a9093712d7fd084bbef87879e7e241cbdd5a2bdafbbc0afaf745d730d655ca70225c2bb0f6f76bdc95d3ed0bac9ea1d54e7cd6f599100bd6ad82be4897abfe33832f8889f748ab5c6081edb3d77a672fe46bb4ef878e1a76fefb190282d97f890ea6c8707dec0747adfc07dbe99e730c392b53fef3a54f5b72f615f7a73c31cc6909d178b9a64da4dc79cc33e0a6bec5ff869584cb048967efc8705946115785193df89ae0b8059c4648560259f84b6096e1758d8c5f165cc70c4eeecab307ceb559b8c49fdaf1098"
);

codec_fixtures!(
    EncapsulatedBlendingHeader,
    Self::initialize(&BlendingHeader::pseudo_random(&[1u8; 32])) => "60fc83e36a86f41f2aab9f027a459f11197167956e98ce4f058ce8b61770a33e428ccf9457b8e0ebcbb53306681ed551f6c9b5858a73bc4ab042267ec3dccb037d45d62adc3b96c7e9be0eaaec656122e540403466651d48b8831e62fd075d0ae9ed034c0f3c715f1f555f60eb52a81b85ba74615266daa42014ca92de139454222992c0a12b6b147c39a7d1eb012c0641abd10f22ad759ef4a4b0b31a067e70ea2bbb898937340b08e76c8fb9dffd2d52203ab0dd8cca727857f9bdc9bdbef60783c768b6466d420efcdad590337e06ce5e85fa534fa1891ded39b430fa62dd7ef62f6bb40dd20e075ba51983a912c51170ab2c4450b3884790a93cb0b2ff2f0a9ba3ee2a1bc09a93088331a1b6995dfde2904c4f61cbaf2e4f7f309ffafb1d00"
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
                PayloadType::BlockProposal,
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
