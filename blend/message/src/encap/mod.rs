use core::num::{NonZeroU64, NonZeroUsize};

use lb_blend_proofs::{
    quota::{ProofOfQuota, VerifiedProofOfQuota},
    selection::{ProofOfSelection, VerifiedProofOfSelection, inputs::VerifyInputs},
};
use lb_key_management_system_keys::keys::Ed25519PublicKey;

use crate::{
    crypto::proofs::PoQVerificationInputsMinusSigningKey,
    message::{
        blending_header::BLENDING_HEADER_ENCODED_SIZE, payload::PAYLOAD_ENCODED_SIZE,
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

/// The number of bytes an encapsulated message can encode to at most on the
/// wire, given the maximum number of per-message encapsulations.
#[must_use]
pub const fn encapsulated_message_encoded_size(num_blend_layers: NonZeroU64) -> NonZeroUsize {
    let blending_headers = BLENDING_HEADER_ENCODED_SIZE
        .checked_mul(num_blend_layers.get() as usize)
        .expect("The encoded size of the blending headers must not overflow.");
    let total = PUBLIC_HEADER_ENCODED_SIZE
        .checked_add(blending_headers)
        .expect("The encoded size of a message must not overflow.")
        .checked_add(PAYLOAD_ENCODED_SIZE)
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
        PayloadType,
        crypto::key_ext::Ed25519SecretKeyExt as _,
        encap::{
            encapsulated::EncapsulatedMessage, encapsulated_message_encoded_size,
            validated::EncapsulatedMessageWithVerifiedPublicHeader,
        },
        input::EncapsulationInput,
    };

    fn encapsulated_message(layers: usize, payload_body: &[u8]) -> EncapsulatedMessage {
        let recipient = UnsecuredEd25519Key::from_bytes(&[1u8; 32]);
        let input = EncapsulationInput::try_new(
            UnsecuredEd25519Key::generate_with_chacha_rng(),
            &recipient.public_key(),
            VerifiedProofOfQuota::from_bytes_unchecked([0; PROOF_OF_QUOTA_SIZE]),
            VerifiedProofOfSelection::from_bytes_unchecked([0; PROOF_OF_SELECTION_SIZE]),
        )
        .expect("the encapsulation input is well formed");

        EncapsulatedMessageWithVerifiedPublicHeader::try_new(
            &[input],
            PayloadType::BlockProposal,
            payload_body.try_into().expect("the payload body fits"),
            layers,
        )
        .expect("the message is well formed")
        .into()
    }

    fn expected_size(layers: u64) -> usize {
        encapsulated_message_encoded_size(NonZeroU64::new(layers).unwrap()).get()
    }

    #[test]
    fn a_message_encodes_to_exactly_the_size_its_layer_count_implies() {
        for layers in 1..=3u64 {
            assert_eq!(
                encapsulated_message(usize::try_from(layers).unwrap(), b"payload").encoded_length(),
                expected_size(layers),
                "A {layers}-layer message did not encode to the size that layer count implies"
            );
        }
    }
}
