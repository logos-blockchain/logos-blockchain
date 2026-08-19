use lb_blend_crypto::fill_random_bytes;
use lb_codec::{BinaryDecode, BinaryEncode, DecodeError, take};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use crate::Error;

/// Every dispersed payload body is padded to this size, so it must fit the
/// largest thing the blend network carries: a block proposal.
///
/// A block proposal is at most 18192 bytes:
/// - A 297-byte header
/// - At most `MAX_UNCLES` uncle headers of 361 bytes each, behind a 1-byte
///   count prefix.
/// - At most `MAX_BLOCK_TXS` transaction references of 16 bytes, behind a
///   2-byte count prefix.
/// - A 64-byte signature
pub const MAX_PAYLOAD_BODY_SIZE: usize = 18_192;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[repr(u8)]
pub enum PayloadType {
    Cover = 0x00,
    BlockProposal = 0x01,
    Transaction = 0x02,
}

impl PayloadType {
    #[must_use]
    pub const fn is_data_message(&self) -> bool {
        matches!(self, Self::BlockProposal | Self::Transaction)
    }
}

impl TryFrom<u8> for PayloadType {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(Self::Cover),
            0x01 => Ok(Self::BlockProposal),
            0x02 => Ok(Self::Transaction),
            _ => Err(()),
        }
    }
}

impl BinaryEncode for PayloadType {
    fn encoded_length(&self) -> usize {
        size_of::<u8>()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        (*self as u8).encode_into(out);
    }
}

impl BinaryDecode for PayloadType {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (remaining, discriminant) = u8::decode(input, &())?;
        let payload_type = Self::try_from(discriminant)
            .map_err(|()| DecodeError::unknown_discriminant::<Self>(u64::from(discriminant)))?;
        Ok((remaining, payload_type))
    }
}

/// The decapsulated payload body, padded to a fixed size with random bytes.
///
/// `actual_len` is the length of the real (unpadded) content and is the single
/// source of truth for it — the payload no longer stores it a second time.
/// Everything past it is padding, and per the Payload Formatting spec
/// (<https://github.com/logos-co/logos-lips/blob/master/docs/blockchain/raw/payload-formatting.md#body>),
/// must be random rather than a fixed filler, so that the body never carries
/// a region of plaintext known to an observer.
#[serde_as]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaddedPayloadBody {
    #[serde(deserialize_with = "deserialize_actual_len")]
    actual_len: u16,

    #[serde_as(as = "serde_with::Bytes")]
    padded: Box<[u8; MAX_PAYLOAD_BODY_SIZE]>,
}

fn deserialize_actual_len<'de, D>(deserializer: D) -> Result<u16, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let actual_len = u16::deserialize(deserializer)?;

    if usize::from(actual_len) > MAX_PAYLOAD_BODY_SIZE {
        return Err(serde::de::Error::custom(format_args!(
            "actual payload length {actual_len} exceeds maximum {MAX_PAYLOAD_BODY_SIZE}"
        )));
    }

    Ok(actual_len)
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

        let actual_len: u16 = value
            .len()
            .try_into()
            .map_err(|_| Error::InvalidPayloadLength)?;

        let mut padded: Box<[u8; MAX_PAYLOAD_BODY_SIZE]> = vec![0; MAX_PAYLOAD_BODY_SIZE]
            .into_boxed_slice()
            .try_into()
            .expect("body must be created with the correct size");
        let padding_start = value.len();
        padded[..padding_start].copy_from_slice(value);
        fill_random_bytes(&mut padded[padding_start..]);

        Ok(Self { actual_len, padded })
    }
}

impl BinaryEncode for PaddedPayloadBody {
    fn encoded_length(&self) -> usize {
        self.actual_len
            .encoded_length()
            .checked_add(MAX_PAYLOAD_BODY_SIZE)
            .unwrap()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.actual_len.encode_into(out);
        out.extend_from_slice(&self.padded[..]);
    }
}

impl BinaryDecode for PaddedPayloadBody {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (input, actual_len) = u16::decode(input, &())?;
        if usize::from(actual_len) > MAX_PAYLOAD_BODY_SIZE {
            return Err(DecodeError::length_out_of_bounds::<Self>(
                usize::from(actual_len),
                0,
                MAX_PAYLOAD_BODY_SIZE,
            ));
        }
        let (body_bytes, remaining) = take::<Self>(input, MAX_PAYLOAD_BODY_SIZE)?;
        let padded: Box<[u8; MAX_PAYLOAD_BODY_SIZE]> = body_bytes
            .to_vec()
            .into_boxed_slice()
            .try_into()
            .expect("Take guarantees the length");
        Ok((remaining, Self { actual_len, padded }))
    }
}

#[cfg(test)]
mod tests {
    use lb_codec::{BinaryDecode as _, BinaryEncode as _};
    use serde::Serialize;
    use serde_with::serde_as;

    use super::*;

    #[serde_as]
    #[derive(Serialize)]
    struct InvalidPaddedPayloadBody {
        actual_len: u16,
        #[serde_as(as = "serde_with::Bytes")]
        padded: Box<[u8; MAX_PAYLOAD_BODY_SIZE]>,
    }

    #[test]
    fn binary_decode_rejects_invalid_actual_length() {
        let actual_len = (MAX_PAYLOAD_BODY_SIZE + 1) as u16;
        let mut encoded = Vec::with_capacity(size_of::<u16>() + MAX_PAYLOAD_BODY_SIZE);
        actual_len.encode_into(&mut encoded);
        encoded.resize(encoded.capacity(), 0);

        let error = PaddedPayloadBody::decode(&encoded, &()).unwrap_err();
        assert!(matches!(
            error,
            DecodeError::LengthOutOfBounds {
                len,
                max: MAX_PAYLOAD_BODY_SIZE,
                ..
            } if len == MAX_PAYLOAD_BODY_SIZE + 1
        ));
    }

    #[test]
    fn serde_deserialize_rejects_invalid_actual_length() {
        let raw = InvalidPaddedPayloadBody {
            actual_len: (MAX_PAYLOAD_BODY_SIZE + 1) as u16,
            padded: vec![0; MAX_PAYLOAD_BODY_SIZE]
                .into_boxed_slice()
                .try_into()
                .unwrap(),
        };
        let encoded = bincode::serialize(&raw).unwrap();
        let error = bincode::deserialize::<PaddedPayloadBody>(&encoded).unwrap_err();

        assert!(format!("{error}").contains("actual payload length"));
    }
}

/// The exact number of bytes a [`Payload`] encodes to: a fixed enum
/// discriminant, the `u16` body length, and the body padded to
/// [`MAX_PAYLOAD_BODY_SIZE`]. Compile-time constant, so the encapsulated
/// (ciphered) form can be stored as a `Box<[u8; PAYLOAD_ENCODED_SIZE]>`.
pub const PAYLOAD_ENCODED_SIZE: usize =
    size_of::<PayloadType>() + size_of::<u16>() + MAX_PAYLOAD_BODY_SIZE;

/// A payload that is fully decapsulated.
/// This must be encapsulated when being sent to the blend network.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payload {
    payload_type: PayloadType,
    body: PaddedPayloadBody,
}

impl Payload {
    pub const fn new(payload_type: PayloadType, body: PaddedPayloadBody) -> Self {
        Self { payload_type, body }
    }

    pub const fn payload_type(&self) -> PayloadType {
        self.payload_type
    }

    /// Returns the payload body unpadded.
    /// Returns an error if the recorded length exceeds the padded buffer.
    pub fn body(&self) -> Result<&[u8], Error> {
        let len = self.body.actual_len as usize;
        if self.body.padded.len() < len {
            return Err(Error::InvalidPayloadLength);
        }
        Ok(&self.body.padded[..len])
    }

    pub fn try_into_components(self) -> Result<(PayloadType, Vec<u8>), Error> {
        Ok((self.payload_type(), self.body()?.to_vec()))
    }
}

impl BinaryEncode for Payload {
    fn encoded_length(&self) -> usize {
        PAYLOAD_ENCODED_SIZE
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.payload_type.encode_into(out);
        self.body.encode_into(out);
    }
}

impl BinaryDecode for Payload {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (input, payload_type) = PayloadType::decode(input, &())?;
        let (input, body) = PaddedPayloadBody::decode(input, &())?;
        Ok((input, Self { payload_type, body }))
    }
}
