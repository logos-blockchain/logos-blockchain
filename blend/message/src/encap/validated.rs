use derivative::Derivative;
use lb_blend_crypto::random_sized_bytes;
use lb_blend_proofs::{
    quota::{self, VerifiedProofOfQuota},
    selection::inputs::VerifyInputs,
};
use lb_key_management_system_keys::keys::{UnsecuredEd25519Key, X25519PrivateKey};
use serde::{Deserialize, Serialize};

use crate::{
    Error, MessageIdentifier, PaddedPayloadBody, PayloadType,
    crypto::key_ext::Ed25519SecretKeyExt as _,
    encap::{
        ProofsVerifier,
        decapsulated::{DecapsulatedMessage, DecapsulationOutput, PartDecapsulationOutput},
        encapsulated::{EncapsulatedMessage, EncapsulatedPart},
    },
    input::EncapsulationInput,
    message::public_header::{PublicHeaderWithVerifiedSignature, VerifiedPublicHeader},
    reward::BlendingToken,
};

#[derive(Derivative, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[derivative(Debug)]
/// An encapsulated message whose public header signature has been verified.
pub struct EncapsulatedMessageWithVerifiedSignature {
    public_header_with_verified_signature: PublicHeaderWithVerifiedSignature,
    #[derivative(Debug = "ignore")] // too long
    encapsulated_part: EncapsulatedPart,
}

impl EncapsulatedMessageWithVerifiedSignature {
    pub fn try_new(
        inputs: &[EncapsulationInput],
        payload_type: PayloadType,
        payload_body: PaddedPayloadBody,
    ) -> Result<Self, Error> {
        Ok(EncapsulatedMessageWithVerifiedPublicHeader::try_new(
            inputs,
            payload_type,
            payload_body,
        )?
        .into())
    }

    #[must_use]
    pub const fn from_components(
        public_header_with_verified_signature: PublicHeaderWithVerifiedSignature,
        encapsulated_part: EncapsulatedPart,
    ) -> Self {
        Self {
            public_header_with_verified_signature,
            encapsulated_part,
        }
    }

    pub fn verify_proof_of_quota<Verifier>(
        self,
        verifier: &Verifier,
    ) -> Result<EncapsulatedMessageWithVerifiedPublicHeader, Error>
    where
        Verifier: ProofsVerifier,
    {
        let (_, signing_key, proof_of_quota, signature) =
            self.public_header_with_verified_signature.into_components();
        let verified_proof_of_quota = verifier
            .verify_proof_of_quota(proof_of_quota, &signing_key)
            .map_err(|_| Error::ProofOfQuotaVerificationFailed(quota::Error::InvalidProof))?;
        let verified_public_header =
            VerifiedPublicHeader::new(verified_proof_of_quota, signing_key, signature);
        Ok(
            EncapsulatedMessageWithVerifiedPublicHeader::from_components(
                verified_public_header,
                self.encapsulated_part,
            ),
        )
    }

    #[must_use]
    pub const fn id(&self) -> MessageIdentifier {
        self.public_header_with_verified_signature.id()
    }

    #[cfg(any(feature = "unsafe-test-functions", test))]
    pub const fn public_header_mut(&mut self) -> &mut PublicHeaderWithVerifiedSignature {
        &mut self.public_header_with_verified_signature
    }
}

impl From<EncapsulatedMessageWithVerifiedSignature> for EncapsulatedMessage {
    fn from(value: EncapsulatedMessageWithVerifiedSignature) -> Self {
        Self::from_components(
            value.public_header_with_verified_signature.into(),
            value.encapsulated_part,
        )
    }
}

#[derive(Debug, Clone, Copy)]
#[cfg_attr(test, derive(Default))]
/// Required inputs to verify a `PoSel` proof, minus the key nullifier that is
/// retrieved from the verified `PoQ` of the outer Blend layer.
pub struct RequiredProofOfSelectionVerificationInputs {
    pub expected_node_index: u64,
    pub total_membership_size: u64,
}

/// An encapsulated message whose public header has been verified.
#[derive(Derivative, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[derivative(Debug)]
pub struct EncapsulatedMessageWithVerifiedPublicHeader {
    validated_public_header: VerifiedPublicHeader,
    #[derivative(Debug = "ignore")] // too long
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

    pub fn try_new(
        inputs: &[EncapsulationInput],
        payload_type: PayloadType,
        payload_body: PaddedPayloadBody,
    ) -> Result<Self, Error> {
        // Create the encapsulated part.
        let (part, signing_key, proof_of_quota) = inputs.iter().enumerate().fold(
            (
                // Start with an initialized encapsulated part,
                // a random signing key, and proof of quota.
                EncapsulatedPart::try_initialize(inputs, payload_type, payload_body)?,
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

        Ok(Self {
            validated_public_header,
            encapsulated_part: part,
        })
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
        let Some(shared_key) = private_key.derive_shared_key(&signing_key.derive_x25519()) else {
            return Err(Error::InvalidSharedSecret);
        };

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

    #[must_use]
    pub const fn public_header(&self) -> &VerifiedPublicHeader {
        &self.validated_public_header
    }

    #[cfg(any(feature = "unsafe-test-functions", test))]
    pub const fn public_header_mut(&mut self) -> &mut VerifiedPublicHeader {
        &mut self.validated_public_header
    }
}

impl From<EncapsulatedMessageWithVerifiedPublicHeader>
    for EncapsulatedMessageWithVerifiedSignature
{
    fn from(value: EncapsulatedMessageWithVerifiedPublicHeader) -> Self {
        Self::from_components(
            value.validated_public_header.into(),
            value.encapsulated_part,
        )
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
