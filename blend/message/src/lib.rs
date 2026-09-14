pub mod codec;
pub mod crypto;
pub mod encap;
pub mod input;
pub mod reward;

mod error;
mod fixtures;
mod message;

pub use codec::{
    deserialize_encapsulated_message, serialize_encapsulated_message_with_verified_public_header,
    serialize_encapsulated_message_with_verified_signature,
};
pub use encap::encapsulated::MessageIdentifier;
pub use error::Error;
pub use message::payload::{MAX_PAYLOAD_BODY_SIZE, PaddedPayloadBody, PayloadType};
use message::{
    blending_header::BLENDING_HEADER_ENCODED_SIZE, payload::PAYLOAD_ENCODED_SIZE,
    public_header::PUBLIC_HEADER_ENCODED_SIZE,
};

/// The number of bytes an encapsulated message can encode to at most on the
/// wire, given the maximum number of per-message encapsulations.
#[must_use]
pub const fn encapsulated_message_encoded_size(num_blend_layers: usize) -> usize {
    PUBLIC_HEADER_ENCODED_SIZE
        .checked_add(
            BLENDING_HEADER_ENCODED_SIZE
                .checked_mul(num_blend_layers)
                .expect("The encoded size of the blending headers must not overflow."),
        )
        .expect("The encoded size of a message must not overflow.")
        .checked_add(PAYLOAD_ENCODED_SIZE)
        .expect("The encoded size of a message must not overflow.")
}
