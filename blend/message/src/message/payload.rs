use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use crate::Error;

pub const MAX_PAYLOAD_BODY_SIZE: usize = 34 * 1024;

/// A payload header that is fully decapsulated.
/// This must be encapsulated when being sent to the blend network.
#[derive(Clone, Serialize, Deserialize)]
struct PayloadHeader {
    payload_type: PayloadType,
    body_len: u16,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PayloadType {
    Cover = 0x00,
    Data = 0x01,
}

#[serde_as]
#[derive(Clone, Serialize, Deserialize)]
pub struct PaddedPayloadBody {
    /// A body is padded to [`MAX_PAYLOAD_BODY_SIZE`],
    /// Box is used to not allocate a big array on the stack.
    #[serde_as(as = "serde_with::Bytes")]
    padded: Box<[u8; MAX_PAYLOAD_BODY_SIZE]>,
    actual_len: u16,
}

impl TryFrom<Vec<u8>> for PaddedPayloadBody {
    type Error = Error;

    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        Self::try_from(value.as_slice())
    }
}

impl TryFrom<&[u8]> for PaddedPayloadBody {
    type Error = Error;

    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        if value.len() > MAX_PAYLOAD_BODY_SIZE {
            return Err(Error::PayloadTooLarge);
        }

        let body_len: u16 = value
            .len()
            .try_into()
            .map_err(|_| Error::InvalidPayloadLength)?;

        let mut padded: Box<[u8; MAX_PAYLOAD_BODY_SIZE]> = vec![0; MAX_PAYLOAD_BODY_SIZE]
            .into_boxed_slice()
            .try_into()
            .expect("body must be created with the correct size");
        padded[..value.len()].copy_from_slice(value);

        Ok(Self {
            actual_len: body_len,
            padded,
        })
    }
}

/// A payload that is fully decapsulated.
/// This must be encapsulated when being sent to the blend network.
#[serde_as]
#[derive(Clone, Serialize, Deserialize)]
pub struct Payload {
    header: PayloadHeader,
    body: PaddedPayloadBody,
}

impl Payload {
    pub const fn new(payload_type: PayloadType, payload_body: PaddedPayloadBody) -> Self {
        Self {
            header: PayloadHeader {
                payload_type,
                body_len: payload_body.actual_len,
            },
            body: payload_body,
        }
    }

    pub const fn payload_type(&self) -> PayloadType {
        self.header.payload_type
    }

    /// Returns the payload body unpadded.
    /// Returns an error if the payload cannot be read up to the length
    /// specified in the header
    pub fn body(&self) -> Result<&[u8], Error> {
        let len = self.header.body_len as usize;
        if self.body.padded.len() < len {
            return Err(Error::InvalidPayloadLength);
        }
        Ok(&self.body.padded[..len])
    }

    pub fn try_into_components(self) -> Result<(PayloadType, Vec<u8>), Error> {
        Ok((self.payload_type(), self.body()?.to_vec()))
    }
}
