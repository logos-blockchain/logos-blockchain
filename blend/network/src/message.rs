use core::fmt::{self, Debug, Formatter};
use std::sync::Arc;

use lb_blend_message::{
    encap::validated::EncapsulatedMessageWithVerifiedPublicHeader,
    serialize_encapsulated_message_with_verified_public_header,
};

#[derive(Debug, Clone)]
pub struct OutgoingMessage(Arc<[u8]>);

impl From<&EncapsulatedMessageWithVerifiedPublicHeader> for OutgoingMessage {
    fn from(message: &EncapsulatedMessageWithVerifiedPublicHeader) -> Self {
        Self(Arc::from(
            serialize_encapsulated_message_with_verified_public_header(message).as_ref(),
        ))
    }
}

impl OutgoingMessage {
    #[cfg(any(test, feature = "unsafe-test-functions"))]
    pub fn from_bytes<Bytes>(bytes: Bytes) -> Self
    where
        Bytes: AsRef<[u8]>,
    {
        Self(Arc::from(bytes.as_ref()))
    }
}

impl AsRef<[u8]> for OutgoingMessage {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// A frame read off the wire.
pub struct IncomingMessage(Box<[u8]>);

impl From<Box<[u8]>> for IncomingMessage {
    fn from(payload: Box<[u8]>) -> Self {
        Self(payload)
    }
}

impl Debug for IncomingMessage {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "IncomingMessage({} bytes)", self.0.len())
    }
}

impl AsRef<[u8]> for IncomingMessage {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}
