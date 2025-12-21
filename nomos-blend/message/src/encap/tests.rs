use core::convert::Infallible;

use key_management_system_keys::keys::{Ed25519PublicKey, Ed25519Signature, UnsecuredEd25519Key};
use nomos_blend_crypto::keys::X25519PrivateKey;
use nomos_blend_proofs::{
    quota::{ProofOfQuota, VerifiedProofOfQuota, inputs::prove::public::LeaderInputs},
    selection::{ProofOfSelection, VerifiedProofOfSelection, inputs::VerifyInputs},
};
use nomos_core::codec::{DeserializeOp as _, SerializeOp as _};

use crate::{
    Error, PayloadType,
    crypto::{key_ext::Ed25519SecretKeyExt as _, proofs::PoQVerificationInputsMinusSigningKey},
    encap::{
        ProofsVerifier,
        decapsulated::DecapsulationOutput,
        encapsulated::EncapsulatedMessage,
        validated::{
            EncapsulatedMessageWithVerifiedPublicHeader, RequiredProofOfSelectionVerificationInputs,
        },
    },
    input::EncapsulationInput,
    message::payload::MAX_PAYLOAD_BODY_SIZE,
};

struct NeverFailingProofsVerifier;

impl ProofsVerifier for NeverFailingProofsVerifier {
    type Error = Infallible;

    fn new(_public_inputs: PoQVerificationInputsMinusSigningKey) -> Self {
        Self
    }

    fn start_epoch_transition(&mut self, _new_pol_inputs: LeaderInputs) {}

    fn complete_epoch_transition(&mut self) {}

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

    fn start_epoch_transition(&mut self, _new_pol_inputs: LeaderInputs) {}

    fn complete_epoch_transition(&mut self) {}

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

    fn start_epoch_transition(&mut self, _new_pol_inputs: LeaderInputs) {}

    fn complete_epoch_transition(&mut self) {}

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
    let msg = EncapsulatedMessage::from(EncapsulatedMessageWithVerifiedPublicHeader::new(
        &inputs,
        PayloadType::Data,
        PAYLOAD_BODY.try_into().unwrap(),
    ));

    // NOTE: We expect that the decapsulations can be done
    // in the "reverse" order of blend_node_enc_keys.
    // (following the notion in the spec)

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
    assert_eq!(decapsulated_message.payload_type(), PayloadType::Data);
    assert_eq!(decapsulated_message.payload_body(), PAYLOAD_BODY);
}

#[test]
#[should_panic(expected = "Payload too large")]
fn payload_too_long() {
    let (inputs, _) = generate_inputs(1);
    drop(EncapsulatedMessageWithVerifiedPublicHeader::new(
        &inputs,
        PayloadType::Data,
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
        let mut msg = EncapsulatedMessage::from(EncapsulatedMessageWithVerifiedPublicHeader::new(
            &inputs,
            PayloadType::Data,
            PAYLOAD_BODY.try_into().unwrap(),
        ));
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
    use nomos_blend_proofs::quota::Error as PoQError;

    const PAYLOAD_BODY: &[u8] = b"hello";
    let verifier = AlwaysFailingProofOfQuotaVerifier;

    let (inputs, _) = generate_inputs(2);
    let msg = EncapsulatedMessage::from(EncapsulatedMessageWithVerifiedPublicHeader::new(
        &inputs,
        PayloadType::Data,
        PAYLOAD_BODY.try_into().unwrap(),
    ));

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
    use nomos_blend_proofs::selection::Error as PoSelError;

    const PAYLOAD_BODY: &[u8] = b"hello";
    let verifier = AlwaysFailingProofOfSelectionVerifier;

    let (inputs, blend_node_enc_keys) = generate_inputs(2);
    let msg = EncapsulatedMessage::from(EncapsulatedMessageWithVerifiedPublicHeader::new(
        &inputs,
        PayloadType::Data,
        PAYLOAD_BODY.try_into().unwrap(),
    ));
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
    let msg = EncapsulatedMessageWithVerifiedPublicHeader::new(
        &inputs,
        PayloadType::Data,
        b"".as_slice().try_into().unwrap(),
    );
    let serialized_encapsulated_message = msg.to_bytes().unwrap();

    let deserialized_as_unverified =
        EncapsulatedMessage::from_bytes(&serialized_encapsulated_message).unwrap();
    assert_eq!(deserialized_as_unverified, msg.into());
    deserialized_as_unverified
        .verify_public_header(&NeverFailingProofsVerifier)
        .unwrap();
}

fn generate_inputs(cnt: usize) -> (Vec<EncapsulationInput>, Vec<X25519PrivateKey>) {
    let recipient_signing_keys =
        core::iter::repeat_with(UnsecuredEd25519Key::generate_with_blake_rng)
            .take(cnt)
            .collect::<Vec<_>>();
    let inputs = recipient_signing_keys
        .iter()
        .map(|recipient_signing_key| {
            EncapsulationInput::new(
                UnsecuredEd25519Key::generate_with_blake_rng(),
                &recipient_signing_key.public_key(),
                VerifiedProofOfQuota::from_bytes_unchecked([0; _]),
                VerifiedProofOfSelection::from_bytes_unchecked([0; _]),
            )
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
