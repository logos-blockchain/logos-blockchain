use std::num::NonZeroU64;

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
use lb_core::codec::{DeserializeOp as _, SerializeOp};
use lb_key_management_system_keys::keys::X25519PrivateKey;

pub mod core_and_leader;
pub use self::core_and_leader::{
    send::SessionCryptographicProcessor as CoreAndLeaderSenderOnlySessionCryptographicProcessor,
    send_and_receive::SessionCryptographicProcessor as CoreAndLeaderSendAndReceiveSessionCryptographicProcessor,
};
pub mod leader;
pub use self::leader::send::SessionCryptographicProcessor as LeaderSenderOnlySessionCryptographicProcessor;

#[cfg(test)]
mod test_utils;

#[derive(Clone, Derivative)]
#[derivative(Debug)]
pub struct SessionCryptographicProcessorSettings {
    /// The non-ephemeral encryption key (NEK) derived from the secret key
    /// corresponding to the public key registered in the membership (SDP).
    #[derivative(Debug = "ignore")]
    pub non_ephemeral_encryption_key: X25519PrivateKey,
    /// `ß_c`: number of blending operations for each locally generated message.
    pub num_blend_layers: NonZeroU64,
}

#[must_use]
pub fn serialize_encapsulated_message_with_verified_public_header(
    message: &EncapsulatedMessageWithVerifiedPublicHeader,
) -> Vec<u8> {
    serialize_message(message)
}

#[must_use]
pub fn serialize_encapsulated_message_with_verified_signature(
    message: &EncapsulatedMessageWithVerifiedSignature,
) -> Vec<u8> {
    serialize_message(message)
}

fn serialize_message<Message>(message: &Message) -> Vec<u8>
where
    Message: SerializeOp,
{
    message
        .to_bytes()
        .expect("Message should be serializable")
        .to_vec()
}

pub fn deserialize_encapsulated_message(message: &[u8]) -> Result<EncapsulatedMessage, Error> {
    EncapsulatedMessage::from_bytes(message).map_err(|_| Error::MessageDeserializationFailed)
}
