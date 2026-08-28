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
    ).unwrap() => "2b70b8a6701e1fb124ef53aeb30edbfc76f0a9553a244eccb8701b31ce152ebc25fe2a69e4862373850d41acf2677775c548c68be6994be07a892e8fa51392d2c31e68a76603f23640fe3f1773a9d4831180fe01023c4bc77e5d3061ad8c5da8b628f43e02f03186c0cecd6dea071230e86ecc8fbfb49847c2fbfc1690377de8a7b1fbbbb57a531e20c8f7a31b1403cfc99bfd0b6f1a6757cb7c6f7d86cc00b95b02236237583bf2294a8c00e2efbb312dd3b8b8dc07318b3fe42f6d720dd1f7a4884a57593a858ae7a08d8f9ee6b85dc27dea3c85d0acc515267f16baae8903abd259c07aa07536e0729a7f6d5e78d85b49c0d77127f8f11d78317832d86271da62e70be90ee52ce5d8ad3edb0cc25d63ce26614d77a33611cb1ffd51bb882c55"
);

codec_fixtures!(
    EncapsulatedBlendingHeader,
    Self::initialize(&BlendingHeader::pseudo_random(&[1u8; 32])) => "33573c0931c2434ba29b86c026fdcc8cba744100c7bb4bdaf1ef55bbd5e4425a819bf64f4c12d05fd95d24731a53a6703dc4cd9dc044905f956589e89aaa882920c4546147079da7062d1e12fbe0f7754196fdce784cf5af594b49f6bbe0e6e4a630776efe360909612c2044ce97a37c07d1fa9e6bcb46034e32b7e6ca66c67d26ddb8f85f1c4fc844c233f079227766089e3ef4ae662bca0c7a79f70b38785ba2c066418939a7a9edf2080b05a1ef75377b19bf3d468678e371ae2bcb1873480115578dbf551afa1ed7a64d4e2a6c6916f0506347248ca2a1353d4a583dd14b67a76e5ea36d2edbc82de90b7f4ccb16b8e1dcac201459479c72bf75e4752465c68571fb9fa6cc8cd6f742e054bf591e93ac3b63f248187adf323273766ee52100"
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
