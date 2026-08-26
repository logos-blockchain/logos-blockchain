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
