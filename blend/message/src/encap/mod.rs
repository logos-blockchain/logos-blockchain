use core::num::{NonZeroU64, NonZeroUsize};

use lb_blend_proofs::{
    quota::{ProofOfQuota, VerifiedProofOfQuota},
    selection::{ProofOfSelection, VerifiedProofOfSelection, inputs::VerifyInputs},
};
use lb_key_management_system_keys::keys::Ed25519PublicKey;

use crate::{
    crypto::proofs::PoQVerificationInputsMinusSigningKey,
    message::{
        blending_header::BLENDING_HEADER_ENCODED_SIZE, payload::payload_encoded_size,
        public_header::PUBLIC_HEADER_ENCODED_SIZE,
    },
};

pub mod decapsulated;
pub mod encapsulated;
pub mod validated;

#[cfg(test)]
mod tests;

/// An epoch-bound `PoQ` verifier.
pub trait ProofsVerifier {
    type Error;

    /// Create a new proof verifier with the public inputs corresponding to the
    /// current epoch.
    fn new(public_inputs: PoQVerificationInputsMinusSigningKey) -> Self;

    /// Proof of Quota verification logic.
    fn verify_proof_of_quota(
        &self,
        proof: ProofOfQuota,
        signing_key: &Ed25519PublicKey,
    ) -> Result<VerifiedProofOfQuota, Self::Error>;

    /// Proof of Selection verification logic.
    fn verify_proof_of_selection(
        &self,
        proof: ProofOfSelection,
        inputs: &VerifyInputs,
    ) -> Result<VerifiedProofOfSelection, Self::Error>;
}

/// The shape every message of an epoch has on the wire, set by the epoch's era.
///
/// A message carries `num_blend_layers` blending headers (`ß_max`), however
/// many times it is actually encapsulated, and a payload body padded to
/// `payload_body_size`. Messages are built and decoded under it, so that all
/// the messages of an epoch have the same size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageLayout {
    pub num_blend_layers: NonZeroU64,
    pub payload_body_size: u16,
}

/// The number of bytes an encapsulated message of `layout` encodes to on the
/// wire.
#[must_use]
pub fn encapsulated_message_encoded_size(
    MessageLayout {
        num_blend_layers,
        payload_body_size,
    }: MessageLayout,
) -> NonZeroUsize {
    let blending_headers = usize::try_from(num_blend_layers.get())
        .ok()
        .and_then(|layers| BLENDING_HEADER_ENCODED_SIZE.checked_mul(layers))
        .expect("The encoded size of the blending headers must not overflow.");
    let total = PUBLIC_HEADER_ENCODED_SIZE
        .checked_add(blending_headers)
        .expect("The encoded size of a message must not overflow.")
        .checked_add(payload_encoded_size(payload_body_size))
        .expect("The encoded size of a message must not overflow.");

    NonZeroUsize::new(total).expect("The encoded size of a message is greater than `0`.")
}

#[cfg(test)]
mod encoded_size_tests {
    use core::num::NonZeroU64;

    use lb_binary_codec::canonical::BinaryEncode as _;
    use lb_blend_proofs::{
        quota::{PROOF_OF_QUOTA_SIZE, VerifiedProofOfQuota},
        selection::{PROOF_OF_SELECTION_SIZE, VerifiedProofOfSelection},
    };
    use lb_key_management_system_keys::keys::UnsecuredEd25519Key;

    use crate::{
        PaddedPayloadBody, PayloadType,
        crypto::key_ext::Ed25519SecretKeyExt as _,
        encap::{
            MessageLayout, encapsulated::EncapsulatedMessage, encapsulated_message_encoded_size,
            validated::EncapsulatedMessageWithVerifiedPublicHeader,
        },
        input::EncapsulationInput,
    };

    const PAYLOAD_BODY_SIZE: u16 = 64;

    fn encapsulated_message(layers: usize, payload_body: &[u8]) -> EncapsulatedMessage {
        let recipient = UnsecuredEd25519Key::from_bytes(&[1u8; 32]);
        let input = EncapsulationInput::new(
            UnsecuredEd25519Key::generate_with_chacha_rng(),
            &recipient.public_key(),
            VerifiedProofOfQuota::from_bytes_unchecked([0; PROOF_OF_QUOTA_SIZE]),
            VerifiedProofOfSelection::from_bytes_unchecked([0; PROOF_OF_SELECTION_SIZE]),
        );

        EncapsulatedMessageWithVerifiedPublicHeader::try_new(
            &[input],
            PayloadType::BlockProposal,
            PaddedPayloadBody::try_new(payload_body, PAYLOAD_BODY_SIZE)
                .expect("the payload body fits"),
            layers,
        )
        .expect("the message is well formed")
        .into()
    }

    fn layout(layers: u64) -> MessageLayout {
        MessageLayout {
            num_blend_layers: NonZeroU64::new(layers).unwrap(),
            payload_body_size: PAYLOAD_BODY_SIZE,
        }
    }

    #[test]
    fn a_message_encodes_to_exactly_the_size_its_layout_implies() {
        for layers in 1..=3u64 {
            assert_eq!(
                encapsulated_message(usize::try_from(layers).unwrap(), b"payload").encoded_length(),
                encapsulated_message_encoded_size(layout(layers)).get(),
                "A {layers}-layer message did not encode to the size its layout implies"
            );
        }
    }
}
