use lb_binary_codec::canonical::{BinaryDecode, BinaryEncode, DecodeError, codec_fixtures, take};
use lb_blend_crypto::fill_random_bytes;
use lb_core::block::Proposal;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use crate::Error;

/// Every dispersed payload body is padded to this size, so it must fit the
/// largest thing the blend network carries: a block proposal.
///
/// A block proposal is bounded by `Proposal::MAX_ENCODED_SIZE`.
const MAX_PAYLOAD_BODY_SIZE_U16: u16 = {
    assert!(Proposal::MAX_ENCODED_SIZE <= u16::MAX as usize);
    Proposal::MAX_ENCODED_SIZE as u16
};

pub const MAX_PAYLOAD_BODY_SIZE: usize = MAX_PAYLOAD_BODY_SIZE_U16 as usize;

/// The length of the unpadded portion of a payload body.
///
/// Construction guarantees that the length does not exceed
/// [`MAX_PAYLOAD_BODY_SIZE`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u16")]
struct PayloadBodyLen(u16);

impl TryFrom<u16> for PayloadBodyLen {
    type Error = Error;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        if value > MAX_PAYLOAD_BODY_SIZE_U16 {
            return Err(Error::PayloadTooLarge);
        }

        Ok(Self(value))
    }
}

impl TryFrom<usize> for PayloadBodyLen {
    type Error = Error;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        let value = u16::try_from(value).map_err(|_| Error::PayloadTooLarge)?;
        Self::try_from(value)
    }
}

impl From<PayloadBodyLen> for usize {
    fn from(value: PayloadBodyLen) -> Self {
        Self::from(value.0)
    }
}

impl BinaryEncode for PayloadBodyLen {
    fn encoded_length(&self) -> usize {
        self.0.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.0.encode_into(out);
    }
}

impl BinaryDecode for PayloadBodyLen {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (remaining, value) = u16::decode(input, &())?;
        let value = Self::try_from(value).map_err(|_| {
            DecodeError::length_out_of_bounds::<Self>(usize::from(value), 0, MAX_PAYLOAD_BODY_SIZE)
        })?;
        Ok((remaining, value))
    }
}

codec_fixtures!(PayloadBodyLen, PayloadBodyLen(0) => "0000");

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

impl AsRef<str> for PayloadType {
    fn as_ref(&self) -> &str {
        match self {
            Self::Cover => "cover",
            Self::BlockProposal => "block_proposal",
            Self::Transaction => "transaction",
        }
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
    actual_len: PayloadBodyLen,

    #[serde_as(as = "serde_with::Bytes")]
    padded: Box<[u8; MAX_PAYLOAD_BODY_SIZE]>,
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
        let actual_len = PayloadBodyLen::try_from(value.len())?;

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
        let (input, actual_len) = PayloadBodyLen::decode(input, &())?;
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
    use serde::Serialize;
    use serde_with::serde_as;

    use super::*;

    // Malformed-wire tests need MAX_PAYLOAD_BODY_SIZE + 1 to fit in the u16 field.
    const _: () = assert!(MAX_PAYLOAD_BODY_SIZE_U16 < u16::MAX);

    #[serde_as]
    #[derive(Serialize)]
    struct InvalidPaddedPayloadBody {
        actual_len: u16,
        #[serde_as(as = "serde_with::Bytes")]
        padded: Box<[u8; MAX_PAYLOAD_BODY_SIZE]>,
    }

    #[test]
    fn binary_decode_rejects_invalid_actual_length() {
        let actual_len = MAX_PAYLOAD_BODY_SIZE_U16 + 1;
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
            } if len == usize::from(actual_len)
        ));
    }

    #[test]
    fn serde_deserialize_rejects_invalid_actual_length() {
        let raw = InvalidPaddedPayloadBody {
            actual_len: MAX_PAYLOAD_BODY_SIZE_U16 + 1,
            padded: vec![0; MAX_PAYLOAD_BODY_SIZE]
                .into_boxed_slice()
                .try_into()
                .unwrap(),
        };
        let encoded = bincode::serialize(&raw).unwrap();
        let error = bincode::deserialize::<PaddedPayloadBody>(&encoded).unwrap_err();

        assert!(format!("{error}").contains("Payload too large"));
    }

    #[test]
    fn payload_body_len_accepts_maximum_and_rejects_next_length() {
        let maximum = PayloadBodyLen::try_from(MAX_PAYLOAD_BODY_SIZE_U16).unwrap();
        assert_eq!(usize::from(maximum), MAX_PAYLOAD_BODY_SIZE);

        assert!(PayloadBodyLen::try_from(MAX_PAYLOAD_BODY_SIZE + 1).is_err());
    }

    #[test]
    fn serde_deserialize_rejects_invalid_payload_body_len() {
        let invalid_len = MAX_PAYLOAD_BODY_SIZE_U16 + 1;
        let encoded = bincode::serialize(&invalid_len).unwrap();
        let error = bincode::deserialize::<PayloadBodyLen>(&encoded).unwrap_err();

        assert!(format!("{error}").contains("Payload too large"));
    }

    #[test]
    fn serde_payload_body_len_keeps_u16_representation() {
        let length = PayloadBodyLen::try_from(1u16).unwrap();

        assert_eq!(
            bincode::serialize(&length).unwrap(),
            bincode::serialize(&1u16).unwrap()
        );
    }

    #[test]
    fn payload_body_returns_original_unpadded_bytes() {
        let original = b"payload body";
        let body = PaddedPayloadBody::try_from(original.as_slice()).unwrap();
        let payload = Payload::new(PayloadType::Transaction, body);

        assert_eq!(payload.body(), original);

        let (payload_type, body) = payload.into_components();
        assert_eq!(payload_type, PayloadType::Transaction);
        assert_eq!(body, original);
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
    pub fn body(&self) -> &[u8] {
        let len = usize::from(self.body.actual_len);
        &self.body.padded[..len]
    }

    pub fn into_components(self) -> (PayloadType, Vec<u8>) {
        (self.payload_type(), self.body().to_vec())
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
