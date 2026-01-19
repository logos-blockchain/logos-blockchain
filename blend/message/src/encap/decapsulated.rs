use lb_blend_proofs::selection::VerifiedProofOfSelection;

use crate::{
    PayloadType,
    encap::encapsulated::{EncapsulatedMessage, EncapsulatedPart, EncapsulatedPrivateHeader},
    message::{Payload, PublicHeader},
    reward::BlendingToken,
};

/// The output of [`EncapsulatedMessage::decapsulate`]
#[derive(Clone)]
pub enum DecapsulationOutput {
    Incompleted {
        remaining_encapsulated_message: Box<EncapsulatedMessage>,
        blending_token: BlendingToken,
    },
    Completed {
        fully_decapsulated_message: DecapsulatedMessage,
        blending_token: BlendingToken,
    },
}

/// The output of [`EncapsulatedPart::decapsulate`]
pub(super) enum PartDecapsulationOutput {
    Incompleted {
        // Encapsulated part of the next layer.
        encapsulated_part: EncapsulatedPart,
        // Public (unverified) header of the next layer.
        public_header: Box<PublicHeader>,
        // Verified PoSel of the current layer.
        verified_proof_of_selection: VerifiedProofOfSelection,
    },

    Completed {
        payload: Payload,
        verified_proof_of_selection: VerifiedProofOfSelection,
    },
}

#[derive(Clone, Debug)]
pub struct DecapsulatedMessage {
    payload_type: PayloadType,
    payload_body: Vec<u8>,
}

impl DecapsulatedMessage {
    pub(crate) const fn new(payload_type: PayloadType, payload_body: Vec<u8>) -> Self {
        Self {
            payload_type,
            payload_body,
        }
    }

    #[must_use]
    pub const fn payload_type(&self) -> PayloadType {
        self.payload_type
    }

    #[must_use]
    pub fn payload_body(&self) -> &[u8] {
        &self.payload_body
    }

    #[must_use]
    pub fn into_components(self) -> (PayloadType, Vec<u8>) {
        (self.payload_type, self.payload_body)
    }
}

/// The output of [`EncapsulatedPrivateHeader::decapsulate`]
pub(super) enum PrivateHeaderDecapsulationOutput {
    Incompleted {
        // Encapsulated part of the next layer.
        encapsulated_private_header: EncapsulatedPrivateHeader,
        // Public (unverified) header of the next layer.
        public_header: PublicHeader,
        // Verified PoSel of the current layer.
        verified_proof_of_selection: VerifiedProofOfSelection,
    },
    Completed {
        encapsulated_private_header: EncapsulatedPrivateHeader,
        public_header: PublicHeader,
        verified_proof_of_selection: VerifiedProofOfSelection,
    },
}
