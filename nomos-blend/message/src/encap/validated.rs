use key_management_system_keys::keys::UnsecuredEd25519Key;
use nomos_blend_crypto::{keys::X25519PrivateKey, random_sized_bytes};
use nomos_blend_proofs::{quota::VerifiedProofOfQuota, selection::inputs::VerifyInputs};
use serde::{Deserialize, Serialize};

use crate::{
    Error, MessageIdentifier, PaddedPayloadBody, PayloadType,
    crypto::key_ext::{Ed25519PublicKeyExt as _, Ed25519SecretKeyExt as _},
    encap::{
        ProofsVerifier,
        decapsulated::{DecapsulatedMessage, DecapsulationOutput, PartDecapsulationOutput},
        encapsulated::{EncapsulatedMessage, EncapsulatedPart},
    },
    input::EncapsulationInput,
    message::public_header::VerifiedPublicHeader,
    reward::BlendingToken,
};

#[derive(Debug, Clone, Copy)]
#[cfg_attr(test, derive(Default))]
/// Required inputs to verify a `PoSel` proof, minus the key nullifier that is
/// retrieved from the verified `PoQ` of the outer Blend layer.
pub struct RequiredProofOfSelectionVerificationInputs {
    pub expected_node_index: u64,
    pub total_membership_size: u64,
}

/// An encapsulated message whose public header has been verified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub struct EncapsulatedMessageWithVerifiedPublicHeader {
    validated_public_header: VerifiedPublicHeader,
    encapsulated_part: EncapsulatedPart,
}

impl EncapsulatedMessageWithVerifiedPublicHeader {
    #[must_use]
    pub fn from_message_unchecked(message: EncapsulatedMessage) -> Self {
        let (public_header, encapsulated_part) = message.into_components();
        Self::from_components(
            VerifiedPublicHeader::from_header_unchecked(&public_header),
            encapsulated_part,
        )
    }

    #[must_use]
    pub fn new(
        inputs: &[EncapsulationInput],
        payload_type: PayloadType,
        payload_body: PaddedPayloadBody,
    ) -> Self {
        // Create the encapsulated part.
        let (part, signing_key, proof_of_quota) = inputs.iter().enumerate().fold(
            (
                // Start with an initialized encapsulated part,
                // a random signing key, and proof of quota.
                EncapsulatedPart::initialize(inputs, payload_type, payload_body),
                UnsecuredEd25519Key::generate_with_blake_rng(),
                VerifiedProofOfQuota::from_bytes_unchecked(random_sized_bytes()),
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

        // Construct the public header.
        let validated_public_header = VerifiedPublicHeader::new(
            proof_of_quota,
            signing_key.public_key(),
            part.sign(&signing_key),
        );

        Self {
            validated_public_header,
            encapsulated_part: part,
        }
    }

    #[must_use]
    pub const fn from_components(
        validated_public_header: VerifiedPublicHeader,
        encapsulated_part: EncapsulatedPart,
    ) -> Self {
        Self {
            validated_public_header,
            encapsulated_part,
        }
    }

    /// Consume the message to return its components.
    #[must_use]
    pub fn into_components(self) -> (VerifiedPublicHeader, EncapsulatedPart) {
        (self.validated_public_header, self.encapsulated_part)
    }

    #[must_use]
    pub const fn id(&self) -> MessageIdentifier {
        self.validated_public_header.id()
    }

    /// Decapsulates the message using the provided key.
    ///
    /// If the provided key is eligible, returns the following:
    /// - [`DecapsulationOutput::Completed`] if the message was fully
    ///   decapsulated by this call.
    /// - [`DecapsulationOutput::Incompleted`] if the message is still
    ///   encapsulated.
    ///
    /// If not, [`Error::DeserializationFailed`] or
    /// [`Error::ProofOfSelectionVerificationFailed`] will be returned.
    pub fn decapsulate<Verifier>(
        self,
        private_key: &X25519PrivateKey,
        RequiredProofOfSelectionVerificationInputs {
            expected_node_index,
            total_membership_size,
        }: &RequiredProofOfSelectionVerificationInputs,
        verifier: &Verifier,
    ) -> Result<DecapsulationOutput, Error>
    where
        Verifier: ProofsVerifier,
    {
        let (validated_public_header, encapsulated_part) = self.into_components();
        let (_, signing_key, verified_proof_of_quota, _) =
            validated_public_header.into_components();

        // Derive the shared key.
        let shared_key = private_key.derive_shared_key(&signing_key.derive_x25519());

        // Decapsulate the encapsulated part.
        match encapsulated_part.decapsulate(
            &shared_key,
            &VerifyInputs {
                expected_node_index: *expected_node_index,
                key_nullifier: verified_proof_of_quota.key_nullifier(),
                total_membership_size: *total_membership_size,
            },
            verifier,
        )? {
            PartDecapsulationOutput::Incompleted {
                encapsulated_part,
                public_header,
                verified_proof_of_selection,
            } => {
                let blending_token = BlendingToken::new(
                    signing_key,
                    verified_proof_of_quota,
                    verified_proof_of_selection,
                );
                Ok(DecapsulationOutput::Incompleted {
                    remaining_encapsulated_message: Box::new(EncapsulatedMessage::from_components(
                        *public_header,
                        encapsulated_part,
                    )),
                    blending_token,
                })
            }
            PartDecapsulationOutput::Completed {
                payload,
                verified_proof_of_selection,
            } => {
                let (payload_type, payload_body) = payload.try_into_components()?;
                let blending_token = BlendingToken::new(
                    signing_key,
                    verified_proof_of_quota,
                    verified_proof_of_selection,
                );
                Ok(DecapsulationOutput::Completed {
                    fully_decapsulated_message: (DecapsulatedMessage::new(
                        payload_type,
                        payload_body,
                    )),
                    blending_token,
                })
            }
        }
    }

    #[cfg(any(feature = "unsafe-test-functions", test))]
    pub const fn public_header_mut(&mut self) -> &mut VerifiedPublicHeader {
        &mut self.validated_public_header
    }
}

impl From<EncapsulatedMessageWithVerifiedPublicHeader> for EncapsulatedMessage {
    fn from(value: EncapsulatedMessageWithVerifiedPublicHeader) -> Self {
        Self::from_components(
            value.validated_public_header.into(),
            value.encapsulated_part,
        )
    }
}
