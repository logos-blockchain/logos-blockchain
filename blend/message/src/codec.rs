//! Wire helpers for whole encapsulated messages.
//!
//! Thin wrappers over the [`lb_codec`] impls, kept here so a crate that only
//! moves messages around — the network behaviours, say — does not have to
//! depend on anything that knows how to *produce* one.

use core::num::NonZeroU64;

use lb_codec::{BinaryDecode as _, BinaryEncode as _};

use crate::{
    Error,
    encap::{
        encapsulated::EncapsulatedMessage,
        validated::{
            EncapsulatedMessageWithVerifiedPublicHeader, EncapsulatedMessageWithVerifiedSignature,
        },
    },
};

#[must_use]
pub fn serialize_encapsulated_message_with_verified_public_header(
    message: &EncapsulatedMessageWithVerifiedPublicHeader,
) -> Vec<u8> {
    message.encode_to_vec()
}

#[must_use]
pub fn serialize_encapsulated_message_with_verified_signature(
    message: &EncapsulatedMessageWithVerifiedSignature,
) -> Vec<u8> {
    message.encode_to_vec()
}

/// Decodes a whole encapsulated message, rejecting trailing bytes.
///
/// # Errors
///
/// [`Error::MessageDeserializationFailed`] if the input does not decode, or
/// decodes with bytes left over.
pub fn deserialize_encapsulated_message(
    message: &[u8],
    num_blend_layers: &NonZeroU64,
) -> Result<EncapsulatedMessage, Error> {
    let (remaining, deserialized_message) = EncapsulatedMessage::decode(message, num_blend_layers)?;
    if !remaining.is_empty() {
        return Err(Error::MessageDeserializationFailed);
    }
    Ok(deserialized_message)
}
