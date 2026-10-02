//! Wire helpers for whole encapsulated messages.
//!
//! Thin wrappers over the [`lb_binary_codec::canonical`] impls, kept here so a
//! crate that only moves messages around — the network behaviours, say — does
//! not have to depend on anything that knows how to *produce* one.

use core::num::NonZeroU64;

use lb_binary_codec::canonical::{BinaryDecode as _, BinaryEncode as _};
use lb_cryptarchia_engine::era::EraVersion;

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

/// Decodes a whole encapsulated message of an era of `version`, rejecting
/// trailing bytes.
///
/// # Errors
///
/// [`Error::MessageDeserializationFailed`] if the input does not decode, or
/// decodes with bytes left over.
pub fn deserialize_encapsulated_message(
    version: EraVersion,
    message: &[u8],
    num_blend_layers: &NonZeroU64,
) -> Result<EncapsulatedMessage, Error> {
    match version {
        EraVersion::V1 => {
            let (remaining, deserialized_message) =
                EncapsulatedMessage::decode(message, num_blend_layers)?;
            if !remaining.is_empty() {
                return Err(Error::MessageDeserializationFailed);
            }
            Ok(deserialized_message)
        }
    }
}
