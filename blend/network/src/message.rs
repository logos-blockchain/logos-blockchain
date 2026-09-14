use core::{
    error,
    fmt::{self, Debug, Display, Formatter},
};
use std::{
    io::{self, ErrorKind::InvalidInput},
    sync::Arc,
};

use lb_blend_message::{
    encap::validated::EncapsulatedMessageWithVerifiedPublicHeader,
    serialize_encapsulated_message_with_verified_public_header,
};

/// The wire format prefixes every message with its length, encoded in this
/// many bytes.
pub type FrameLength = u16;
pub const FRAME_LENGTH_WIRE_SIZE: usize = size_of::<FrameLength>();

const MAX_FRAME_SIZE: usize = FrameLength::MAX as usize;

/// A message too large for the wire format's length prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTooLarge {
    pub actual: usize,
}

impl Display for FrameTooLarge {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Message length is too big. Got {}, expected at most {MAX_FRAME_SIZE}",
            self.actual
        )
    }
}

impl error::Error for FrameTooLarge {}

impl From<FrameTooLarge> for io::Error {
    fn from(error: FrameTooLarge) -> Self {
        Self::new(InvalidInput, error.to_string())
    }
}

#[derive(Debug, Clone)]
pub struct OutgoingMessage {
    bytes: Arc<[u8]>,
    /// The prefix the wire format leads with, computed when the message was
    /// accepted rather than each time it is sent.
    wire_length: FrameLength,
}

impl TryFrom<&EncapsulatedMessageWithVerifiedPublicHeader> for OutgoingMessage {
    type Error = FrameTooLarge;

    fn try_from(
        message: &EncapsulatedMessageWithVerifiedPublicHeader,
    ) -> Result<Self, FrameTooLarge> {
        Self::try_from_bytes(serialize_encapsulated_message_with_verified_public_header(
            message,
        ))
    }
}

impl OutgoingMessage {
    pub fn try_from_bytes<MsgBytes>(bytes: MsgBytes) -> Result<Self, FrameTooLarge>
    where
        MsgBytes: AsRef<[u8]>,
    {
        let input_length = bytes.as_ref().len();
        let wire_length = FrameLength::try_from(input_length).map_err(|_| FrameTooLarge {
            actual: input_length,
        })?;
        Ok(Self {
            wire_length,
            bytes: Arc::from(bytes.as_ref()),
        })
    }

    #[must_use]
    pub const fn wire_length(&self) -> FrameLength {
        self.wire_length
    }
}

impl AsRef<[u8]> for OutgoingMessage {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
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

#[cfg(test)]
mod tests {
    use crate::message::{FrameTooLarge, MAX_FRAME_SIZE, OutgoingMessage};

    #[test]
    fn a_message_the_wire_format_can_frame_is_accepted() {
        let message = OutgoingMessage::try_from_bytes(b"payload").unwrap();

        assert_eq!(message.as_ref(), b"payload");
        assert_eq!(usize::from(message.wire_length()), b"payload".len());
    }

    #[test]
    fn a_message_too_large_to_frame_is_refused_when_it_is_built() {
        let oversized = vec![0u8; MAX_FRAME_SIZE + 1];

        assert_eq!(
            OutgoingMessage::try_from_bytes(oversized).unwrap_err(),
            FrameTooLarge {
                actual: MAX_FRAME_SIZE + 1
            }
        );
    }
}
