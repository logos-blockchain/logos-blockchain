use std::{num::NonZeroU64, sync::Arc};

use derivative::Derivative;
use lb_blend_message::{
    Error,
    encap::{
        encapsulated::EncapsulatedMessage,
        validated::{
            EncapsulatedMessageWithVerifiedPublicHeader, EncapsulatedMessageWithVerifiedSignature,
        },
    },
};
use lb_codec::{BinaryDecode as _, BinaryEncode as _};
use lb_key_management_system_keys::keys::X25519PrivateKey;
use rayon::ThreadPool;

pub mod core_and_leader;
pub use self::core_and_leader::{
    receive::EpochCryptographicProcessor as CoreAndLeaderReceiverOnlyEpochCryptographicProcessor,
    send::EpochCryptographicProcessor as CoreAndLeaderSenderOnlyEpochCryptographicProcessor,
    send_and_receive::EpochCryptographicProcessor as CoreAndLeaderSendAndReceiveEpochCryptographicProcessor,
};
pub mod leader;
pub use self::leader::send::EpochCryptographicProcessor as LeaderSenderOnlyEpochCryptographicProcessor;

#[cfg(test)]
mod test_utils;

#[derive(Clone, Derivative)]
#[derivative(Debug)]
pub struct EpochCryptographicProcessorSettings {
    /// The non-ephemeral encryption key (NEK) derived from the secret key
    /// corresponding to the public key registered in the membership (SDP).
    #[derivative(Debug = "ignore")]
    pub non_ephemeral_encryption_key: X25519PrivateKey,
    /// `ß_c`: number of blending operations for each locally generated message.
    pub num_blend_layers: NonZeroU64,
    /// The dedicated thread pool the `PoW` puzzle search runs on, built once
    /// for the service and shared by every epoch's processor.
    pub pow_mining_pool: Arc<ThreadPool>,
}

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
