use core::convert::Infallible;

use lb_blend_proofs::{
    quota::{ProofOfQuota, VerifiedProofOfQuota},
    selection::{ProofOfSelection, VerifiedProofOfSelection, inputs::VerifyInputs},
};
use lb_codec::{BinaryDecode as _, BinaryEncode as _};
use lb_core::codec::{DeserializeOp as _, SerializeOp as _};
use lb_key_management_system_keys::keys::{
    Ed25519PublicKey, Ed25519Signature, UnsecuredEd25519Key, X25519PrivateKey,
};

use crate::{
    Error, PaddedPayloadBody, PayloadType,
    crypto::{key_ext::Ed25519SecretKeyExt as _, proofs::PoQVerificationInputsMinusSigningKey},
    encap::{
        ProofsVerifier,
        decapsulated::DecapsulationOutput,
        encapsulated::{EncapsulatedMessage, EncapsulatedPart},
        validated::{
            EncapsulatedMessageWithVerifiedPublicHeader, EncapsulatedMessageWithVerifiedSignature,
            RequiredProofOfSelectionVerificationInputs,
        },
    },
    input::EncapsulationInput,
    message::{payload::MAX_PAYLOAD_BODY_SIZE, public_header::VerifiedPublicHeader},
};

struct NeverFailingProofsVerifier;

impl ProofsVerifier for NeverFailingProofsVerifier {
    type Error = Infallible;

    fn new(_public_inputs: PoQVerificationInputsMinusSigningKey) -> Self {
        Self
    }

    fn verify_proof_of_quota(
        &self,
        proof: ProofOfQuota,
        _signing_key: &Ed25519PublicKey,
    ) -> Result<VerifiedProofOfQuota, Self::Error> {
        Ok(VerifiedProofOfQuota::from_proof_of_quota_unchecked(proof))
    }

    fn verify_proof_of_selection(
        &self,
        proof: ProofOfSelection,
        _inputs: &VerifyInputs,
    ) -> Result<VerifiedProofOfSelection, Self::Error> {
        Ok(VerifiedProofOfSelection::from_proof_of_selection_unchecked(
            proof,
        ))
    }
}

struct AlwaysFailingProofOfQuotaVerifier;

impl ProofsVerifier for AlwaysFailingProofOfQuotaVerifier {
    type Error = ();

    fn new(_public_inputs: PoQVerificationInputsMinusSigningKey) -> Self {
        Self
    }

    fn verify_proof_of_quota(
        &self,
        _proof: ProofOfQuota,
        _signing_key: &Ed25519PublicKey,
    ) -> Result<VerifiedProofOfQuota, Self::Error> {
        Err(())
    }

    fn verify_proof_of_selection(
        &self,
        proof: ProofOfSelection,
        _inputs: &VerifyInputs,
    ) -> Result<VerifiedProofOfSelection, Self::Error> {
        Ok(VerifiedProofOfSelection::from_proof_of_selection_unchecked(
            proof,
        ))
    }
}

struct AlwaysFailingProofOfSelectionVerifier;

impl ProofsVerifier for AlwaysFailingProofOfSelectionVerifier {
    type Error = ();

    fn new(_public_inputs: PoQVerificationInputsMinusSigningKey) -> Self {
        Self
    }

    fn verify_proof_of_quota(
        &self,
        proof: ProofOfQuota,
        _signing_key: &Ed25519PublicKey,
    ) -> Result<VerifiedProofOfQuota, Self::Error> {
        Ok(VerifiedProofOfQuota::from_proof_of_quota_unchecked(proof))
    }

    fn verify_proof_of_selection(
        &self,
        _proof: ProofOfSelection,
        _inputs: &VerifyInputs,
    ) -> Result<VerifiedProofOfSelection, Self::Error> {
        Err(())
    }
}

#[test]
fn encapsulate_and_decapsulate() {
    const PAYLOAD_BODY: &[u8] = b"hello";
    let verifier = NeverFailingProofsVerifier;

    let (inputs, blend_node_enc_keys) = generate_inputs(2);
    let msg = EncapsulatedMessage::from(
        try_new_fully_encapsulated(
            &inputs,
            PayloadType::BlockProposal,
            PAYLOAD_BODY.try_into().unwrap(),
        )
        .unwrap(),
    );

    // NOTE: We expect that the decapsulations can be done
    // in the "reverse" order of blend_node_enc_keys.
    // (following the spec)

    // We can decapsulate with the correct private key.
    let DecapsulationOutput::Incompleted {
        remaining_encapsulated_message: msg,
        ..
    } = msg
        .verify_public_header(&verifier)
        .unwrap()
        .decapsulate(
            blend_node_enc_keys.last().unwrap(),
            &RequiredProofOfSelectionVerificationInputs::default(),
            &verifier,
        )
        .unwrap()
    else {
        panic!("Expected an incompleted message");
    };

    // We cannot decapsulate with an invalid private key,
    // which we already used for the first decapsulation.
    assert!(
        msg.clone()
            .verify_public_header(&verifier)
            .unwrap()
            .decapsulate(
                blend_node_enc_keys.last().unwrap(),
                &RequiredProofOfSelectionVerificationInputs::default(),
                &verifier,
            )
            .is_err()
    );

    // We can decapsulate with the correct private key
    // and the fully-decapsulated payload is correct.
    let DecapsulationOutput::Completed {
        fully_decapsulated_message: decapsulated_message,
        ..
    } = msg
        .verify_public_header(&verifier)
        .unwrap()
        .decapsulate(
            blend_node_enc_keys.first().unwrap(),
            &RequiredProofOfSelectionVerificationInputs::default(),
            &verifier,
        )
        .unwrap()
    else {
        panic!("Expected an incompleted message");
    };
    // The payload body should be the same as the original one.
    assert_eq!(
        decapsulated_message.payload_type(),
        PayloadType::BlockProposal
    );
    assert_eq!(decapsulated_message.payload_body(), PAYLOAD_BODY);
}

#[test]
#[should_panic(expected = "Payload too large")]
fn payload_too_long() {
    let (inputs, _) = generate_inputs(1);
    drop(try_new_fully_encapsulated(
        &inputs,
        PayloadType::BlockProposal,
        vec![0u8; MAX_PAYLOAD_BODY_SIZE + 1]
            .try_into()
            .expect("Payload too large"),
    ));
}

#[test]
fn invalid_public_header_signature() {
    const PAYLOAD_BODY: &[u8] = b"hello";
    let verifier = NeverFailingProofsVerifier;

    let msg_with_invalid_signature = {
        let (inputs, _) = generate_inputs(2);
        let mut msg = EncapsulatedMessage::from(
            try_new_fully_encapsulated(
                &inputs,
                PayloadType::BlockProposal,
                PAYLOAD_BODY.try_into().unwrap(),
            )
            .unwrap(),
        );
        *msg.public_header_mut().signature_mut() = Ed25519Signature::from([100u8; _]);
        msg
    };

    let public_header_verification_result =
        msg_with_invalid_signature.verify_public_header(&verifier);
    assert!(matches!(
        public_header_verification_result,
        Err(Error::SignatureVerificationFailed)
    ));
}

#[test]
fn invalid_public_header_proof_of_quota() {
    use lb_blend_proofs::quota::Error as PoQError;

    const PAYLOAD_BODY: &[u8] = b"hello";
    let verifier = AlwaysFailingProofOfQuotaVerifier;

    let (inputs, _) = generate_inputs(2);
    let msg = EncapsulatedMessage::from(
        try_new_fully_encapsulated(
            &inputs,
            PayloadType::BlockProposal,
            PAYLOAD_BODY.try_into().unwrap(),
        )
        .unwrap(),
    );

    let public_header_verification_result = msg.verify_public_header(&verifier);
    assert!(matches!(
        public_header_verification_result,
        Err(Error::ProofOfQuotaVerificationFailed(
            PoQError::InvalidProof
        ))
    ));
}

#[test]
fn invalid_blend_header_proof_of_selection() {
    use lb_blend_proofs::selection::Error as PoSelError;

    const PAYLOAD_BODY: &[u8] = b"hello";
    let verifier = AlwaysFailingProofOfSelectionVerifier;

    let (inputs, blend_node_enc_keys) = generate_inputs(2);
    let msg = EncapsulatedMessage::from(
        try_new_fully_encapsulated(
            &inputs,
            PayloadType::BlockProposal,
            PAYLOAD_BODY.try_into().unwrap(),
        )
        .unwrap(),
    );
    let validated_message = msg.verify_public_header(&verifier).unwrap();

    let validated_message_decapsulation_result = validated_message.decapsulate(
        blend_node_enc_keys.last().unwrap(),
        &RequiredProofOfSelectionVerificationInputs::default(),
        &verifier,
    );
    assert!(matches!(
        validated_message_decapsulation_result,
        Err(Error::ProofOfSelectionVerificationFailed(
            PoSelError::Verification
        ))
    ));
}

#[test]
fn serde_encapsulated_and_verified() {
    let (inputs, _) = generate_inputs(3);
    let msg = try_new_fully_encapsulated(
        &inputs,
        PayloadType::BlockProposal,
        b"".as_slice().try_into().unwrap(),
    )
    .unwrap();
    let serialized_encapsulated_message = msg.to_bytes().unwrap();

    let deserialized_as_unverified =
        EncapsulatedMessage::from_bytes(&serialized_encapsulated_message).unwrap();
    assert_eq!(deserialized_as_unverified, msg.into());
    deserialized_as_unverified
        .verify_public_header(&NeverFailingProofsVerifier)
        .unwrap();
}

#[test]
fn encapsulate_and_decapsulate_via_two_step_verification() {
    const PAYLOAD_BODY: &[u8] = b"hello";
    let verifier = NeverFailingProofsVerifier;

    let (inputs, blend_node_enc_keys) = generate_inputs(2);
    let msg = EncapsulatedMessage::from(
        try_new_fully_encapsulated(
            &inputs,
            PayloadType::BlockProposal,
            PAYLOAD_BODY.try_into().unwrap(),
        )
        .unwrap(),
    );

    // Step 1: verify signature (forwarding would happen here)
    let sig_verified = msg.verify_header_signature().unwrap();

    // Step 2: verify PoQ (the service layer does this before decapsulation)
    let fully_verified = sig_verified.verify_proof_of_quota(&verifier).unwrap();

    // Step 3: decapsulate
    let DecapsulationOutput::Incompleted {
        remaining_encapsulated_message: msg,
        ..
    } = fully_verified
        .decapsulate(
            blend_node_enc_keys.last().unwrap(),
            &RequiredProofOfSelectionVerificationInputs::default(),
            &verifier,
        )
        .unwrap()
    else {
        panic!("Expected an incompleted message");
    };

    let DecapsulationOutput::Completed {
        fully_decapsulated_message,
        ..
    } = msg
        .verify_public_header(&verifier)
        .unwrap()
        .decapsulate(
            blend_node_enc_keys.first().unwrap(),
            &RequiredProofOfSelectionVerificationInputs::default(),
            &verifier,
        )
        .unwrap()
    else {
        panic!("Expected a completed message");
    };

    assert_eq!(
        fully_decapsulated_message.payload_type(),
        PayloadType::BlockProposal
    );
    assert_eq!(fully_decapsulated_message.payload_body(), PAYLOAD_BODY);
}

#[test]
fn empty_inputs_returns_error() {
    assert!(matches!(
        try_new_fully_encapsulated(
            &[],
            PayloadType::BlockProposal,
            b"hello".as_slice().try_into().unwrap(),
        ),
        Err(Error::EmptyEncapsulationInputs)
    ));
}

#[test]
fn more_inputs_than_layers_returns_error() {
    let (inputs, _) = generate_inputs(4);
    assert!(matches!(
        EncapsulatedMessageWithVerifiedPublicHeader::try_new(
            &inputs,
            PayloadType::BlockProposal,
            b"hello".as_slice().try_into().unwrap(),
            3
        ),
        Err(Error::EncapsulationCountExceeded)
    ));
}

#[test]
fn encapsulate_and_decapsulate_fewer_layers_than_maximum() {
    // A message encapsulated `h` times still carries `ß_max` blending headers;
    // the unused ones hold random filler. Decapsulating it `h` times must yield
    // the original payload, i.e. the filler must not disturb the shift-and-
    // reconstruct invariant that the per-layer signatures depend on.
    const PAYLOAD_BODY: &[u8] = b"hello";
    const MAX_LAYERS: usize = 4;
    let verifier = NeverFailingProofsVerifier;

    for used_layers in 1..=MAX_LAYERS {
        let (inputs, blend_node_enc_keys) = generate_inputs(used_layers);
        let mut msg = EncapsulatedMessage::from(
            EncapsulatedMessageWithVerifiedPublicHeader::try_new(
                &inputs,
                PayloadType::BlockProposal,
                PAYLOAD_BODY.try_into().unwrap(),
                MAX_LAYERS,
            )
            .unwrap(),
        );

        // Regardless of `used_layers`, the message is the size of a `MAX_LAYERS`
        // one — the encapsulation count never reaches the wire.
        assert_eq!(
            msg.encode().len(),
            msg.encoded_length(),
            "message with {used_layers} used layer(s) has the wrong size"
        );

        // Decapsulate in the reverse order of `blend_node_enc_keys`.
        for (hop, key) in blend_node_enc_keys.iter().enumerate().rev() {
            let output = msg
                .clone()
                .verify_public_header(&verifier)
                .unwrap()
                .decapsulate(
                    key,
                    &RequiredProofOfSelectionVerificationInputs::default(),
                    &verifier,
                )
                .unwrap();
            match output {
                DecapsulationOutput::Incompleted {
                    remaining_encapsulated_message,
                    ..
                } => {
                    assert_ne!(hop, 0, "the innermost layer should complete the message");
                    msg = *remaining_encapsulated_message;
                }
                DecapsulationOutput::Completed {
                    fully_decapsulated_message,
                    ..
                } => {
                    assert_eq!(hop, 0, "only the innermost layer should complete");
                    assert_eq!(
                        fully_decapsulated_message.payload_type(),
                        PayloadType::BlockProposal
                    );
                    assert_eq!(fully_decapsulated_message.payload_body(), PAYLOAD_BODY);
                }
            }
        }
    }
}

#[test]
fn payload_body_is_padded_with_random_data() {
    // The padding past `body_length` must be random, not a fixed filler, so two
    // payloads with the same content differ past the content.
    const BODY: &[u8] = b"hello";
    let pad_of = |body: &[u8]| {
        let mut encoded = Vec::new();
        PaddedPayloadBody::try_from(body)
            .unwrap()
            .encode_into(&mut encoded);
        // Skip the `u16` length prefix and the body itself.
        encoded.split_off(size_of::<u16>() + body.len())
    };

    let (first, second) = (pad_of(BODY), pad_of(BODY));
    assert_eq!(first.len(), MAX_PAYLOAD_BODY_SIZE - BODY.len());
    assert_ne!(first, second);
    assert!(
        first.iter().any(|&byte| byte != 0),
        "padding must not be a fixed filler"
    );
}

#[test]
fn payload_body_round_trips_through_encapsulation() {
    // Random padding must not disturb the body itself: what comes out of a full
    // decapsulation is exactly what went in, with no padding bleeding into it.
    const BODY: &[u8] = b"hello";
    let verifier = NeverFailingProofsVerifier;

    let (inputs, blend_node_enc_keys) = generate_inputs(1);
    let DecapsulationOutput::Completed {
        fully_decapsulated_message,
        ..
    } = try_new_fully_encapsulated(
        &inputs,
        PayloadType::BlockProposal,
        BODY.try_into().unwrap(),
    )
    .unwrap()
    .decapsulate(
        blend_node_enc_keys.first().unwrap(),
        &RequiredProofOfSelectionVerificationInputs::default(),
        &verifier,
    )
    .unwrap()
    else {
        panic!("Expected a completed message");
    };
    assert_eq!(fully_decapsulated_message.payload_body(), BODY);
}

#[test]
fn filler_layers_are_not_reconstructable() {
    // Two messages built from the very same inputs must differ in their filler
    // layers: those come from fresh entropy, not from the shared keys, so no
    // observer (nor the sender, after the fact) can regenerate them.
    let (inputs, _) = generate_inputs(1);
    let encode = || {
        EncapsulatedMessage::from(
            EncapsulatedMessageWithVerifiedPublicHeader::try_new(
                &inputs,
                PayloadType::BlockProposal,
                b"hello".as_slice().try_into().unwrap(),
                3,
            )
            .unwrap(),
        )
        .encode()
    };
    assert_ne!(encode(), encode());
}

#[test]
fn decapsulate_empty_private_headers_returns_error() {
    let msg = {
        let part = EncapsulatedPart::new_unchecked(
            // Empty inputs
            &[],
            PayloadType::BlockProposal,
            b"hello".as_slice().try_into().unwrap(),
            // ...and no filler layers either, so the private header is empty.
            0,
        );
        let verified_public_header = VerifiedPublicHeader::new(
            VerifiedProofOfQuota::from_bytes_unchecked([0; _]),
            UnsecuredEd25519Key::generate_with_chacha_rng().public_key(),
            [0u8; _].into(),
        );
        EncapsulatedMessageWithVerifiedPublicHeader::from_components(verified_public_header, part)
    };
    let result = msg.decapsulate(
        // Dummy private key
        &[0; _].into(),
        &RequiredProofOfSelectionVerificationInputs::default(),
        &NeverFailingProofsVerifier,
    );
    assert!(matches!(result, Err(Error::EmptyEncapsulationInputs)));
}

fn sample_message(num_layers: usize) -> EncapsulatedMessageWithVerifiedPublicHeader {
    let (inputs, _) = generate_inputs(num_layers);
    try_new_fully_encapsulated(
        &inputs,
        PayloadType::BlockProposal,
        b"payload".as_slice().try_into().unwrap(),
    )
    .unwrap()
}

#[test]
fn serialized_size_constants_match_wire_format() {
    // The O(1) size gate in `deserialize_from_remote` relies on
    // `expected_serialized_len` being exact. Build real, genuinely-encapsulated
    // messages of varying layer counts and confirm the constant-derived length
    // matches the actual encoded length — this pins every size constant to the
    // real wire encoding.
    for num_layers in 1..=4u64 {
        let message = EncapsulatedMessage::from(sample_message(num_layers as usize));

        let actual_len = message.encode().len();
        let expected_len = message.encoded_length();

        assert_eq!(
            expected_len, actual_len,
            "expected_serialized_len mismatch for {num_layers} layer(s)"
        );
    }
}

#[test]
fn encode_decode_round_trip() {
    // A message encoded to the wire format and decoded back with the expected
    // layer count reconstructs the original.
    for num_layers in 1..=4u64 {
        let message = EncapsulatedMessage::from(sample_message(num_layers as usize));

        let encoded = message.encode();
        let (remaining, decoded) =
            EncapsulatedMessage::decode(&encoded, &num_layers.try_into().unwrap()).unwrap();

        assert!(
            remaining.is_empty(),
            "leftover bytes for {num_layers} layer(s)"
        );
        assert_eq!(
            decoded, message,
            "round-trip mismatch for {num_layers} layer(s)"
        );
    }
}

#[test]
fn wire_bytes_identical_across_message_types() {
    // The send path serializes a verified variant; the receiver decodes an
    // `EncapsulatedMessage`. All three must produce byte-identical wire output.
    let with_public_header = sample_message(3);
    let with_signature: EncapsulatedMessageWithVerifiedSignature =
        with_public_header.clone().into();
    let unverified = EncapsulatedMessage::from(with_public_header.clone());

    let bytes = with_public_header.encode();
    assert_eq!(with_signature.encode(), bytes);
    assert_eq!(unverified.encode(), bytes);
}

// Rejecting a message whose layer count differs from the expected one is now
// the responsibility of the network-side size gate (it compares the received
// length against `EncapsulatedMessage::expected_serialized_len`), covered by
// the `blend-network` tests. `decode` itself assumes a correctly-sized input.

/// Encapsulate with `ß_max` equal to the number of inputs, i.e. the message
/// uses up every layer it carries and has no random filler.
fn try_new_fully_encapsulated(
    inputs: &[EncapsulationInput],
    payload_type: PayloadType,
    payload_body: PaddedPayloadBody,
) -> Result<EncapsulatedMessageWithVerifiedPublicHeader, Error> {
    EncapsulatedMessageWithVerifiedPublicHeader::try_new(
        inputs,
        payload_type,
        payload_body,
        inputs.len(),
    )
}

fn generate_inputs(cnt: usize) -> (Vec<EncapsulationInput>, Vec<X25519PrivateKey>) {
    let recipient_signing_keys =
        core::iter::repeat_with(UnsecuredEd25519Key::generate_with_chacha_rng)
            .take(cnt)
            .collect::<Vec<_>>();
    let inputs = recipient_signing_keys
        .iter()
        .map(|recipient_signing_key| {
            EncapsulationInput::try_new(
                UnsecuredEd25519Key::generate_with_chacha_rng(),
                &recipient_signing_key.public_key(),
                VerifiedProofOfQuota::from_bytes_unchecked([0; _]),
                VerifiedProofOfSelection::from_bytes_unchecked([0; _]),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    (
        inputs,
        recipient_signing_keys
            .iter()
            .map(UnsecuredEd25519Key::derive_x25519)
            .collect(),
    )
}
