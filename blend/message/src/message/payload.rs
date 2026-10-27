use lb_binary_codec::canonical::{BinaryDecode, BinaryEncode, DecodeError, codec_fixtures, take};
use lb_blend_crypto::fill_random_bytes;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use serde_with::serde_as;

use crate::Error;

/// The length of the unpadded portion of a payload body, at most the size the
/// body is padded to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
struct PayloadBodyLen(u16);

impl PayloadBodyLen {
    /// The length of `len` bytes of content, in a body padded to `body_size`.
    fn try_new(len: usize, body_size: u16) -> Result<Self, Error> {
        let len = u16::try_from(len).map_err(|_| Error::PayloadTooLarge)?;
        if len > body_size {
            return Err(Error::PayloadTooLarge);
        }

        Ok(Self(len))
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
    /// The size the body is padded to, which the length is held to.
    type Context = u16;

    fn decode<'input>(
        input: &'input [u8],
        body_size: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (remaining, len) = u16::decode(input, &())?;
        if len > *body_size {
            return Err(DecodeError::length_out_of_bounds::<Self>(
                usize::from(len),
                0,
                usize::from(*body_size),
            ));
        }
        Ok((remaining, Self(len)))
    }
}

codec_fixtures!(PayloadBodyLen, context = 0, PayloadBodyLen(0) => "0000");

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

/// The decapsulated payload body, padded with random bytes to the payload body
/// size of its era.
///
/// `actual_len` is the length of the real (unpadded) content and is the single
/// source of truth for it — the payload no longer stores it a second time.
/// Everything past it is padding, and per the Payload Formatting spec
/// (<https://github.com/logos-co/logos-lips/blob/master/docs/blockchain/raw/payload-formatting.md#body>),
/// must be random rather than a fixed filler, so that the body never carries
/// a region of plaintext known to an observer.
///
/// Every body of an era has the era's size, the largest thing the blend
/// network carries in it (its largest block proposal), so that messages cannot
/// be told apart by their size. The size is the context the body decodes
/// under.
#[serde_as]
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PaddedPayloadBody {
    actual_len: PayloadBodyLen,

    #[serde_as(as = "serde_with::Bytes")]
    padded: Box<[u8]>,
}

impl PaddedPayloadBody {
    /// Pads `content` with random bytes to `body_size`, the payload body size
    /// of the era the body is sent in.
    ///
    /// # Errors
    ///
    /// [`Error::PayloadTooLarge`] if `content` is longer than `body_size`.
    pub fn try_new(content: &[u8], body_size: u16) -> Result<Self, Error> {
        let actual_len = PayloadBodyLen::try_new(content.len(), body_size)?;

        let mut padded = vec![0; usize::from(body_size)].into_boxed_slice();
        let padding_start = content.len();
        padded[..padding_start].copy_from_slice(content);
        fill_random_bytes(&mut padded[padding_start..]);

        Ok(Self { actual_len, padded })
    }
}

// Serde reads are trusted (the node's own recovery state): the body keeps the
// size it was stored with, and only its content length is checked against it.
impl<'de> Deserialize<'de> for PaddedPayloadBody {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[serde_as]
        #[derive(Deserialize)]
        struct RawPaddedPayloadBody {
            actual_len: PayloadBodyLen,
            #[serde_as(as = "serde_with::Bytes")]
            padded: Box<[u8]>,
        }

        let RawPaddedPayloadBody { actual_len, padded } =
            RawPaddedPayloadBody::deserialize(deserializer)?;
        let body_size =
            u16::try_from(padded.len()).map_err(|_| D::Error::custom(Error::PayloadTooLarge))?;
        if actual_len.0 > body_size {
            return Err(D::Error::custom(Error::PayloadTooLarge));
        }

        Ok(Self { actual_len, padded })
    }
}

impl BinaryEncode for PaddedPayloadBody {
    fn encoded_length(&self) -> usize {
        self.actual_len
            .encoded_length()
            .checked_add(self.padded.len())
            .unwrap()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.actual_len.encode_into(out);
        out.extend_from_slice(&self.padded);
    }
}

impl BinaryDecode for PaddedPayloadBody {
    /// The size the body is padded to.
    type Context = u16;

    fn decode<'input>(
        input: &'input [u8],
        body_size: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (input, actual_len) = PayloadBodyLen::decode(input, body_size)?;
        let (body_bytes, remaining) = take::<Self>(input, usize::from(*body_size))?;
        Ok((
            remaining,
            Self {
                actual_len,
                padded: body_bytes.into(),
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A payload body size for the tests.
    const BODY_SIZE: u16 = 64;

    #[test]
    fn binary_decode_rejects_a_length_past_the_body_size() {
        let actual_len = BODY_SIZE + 1;
        let mut encoded = Vec::with_capacity(size_of::<u16>() + usize::from(BODY_SIZE));
        actual_len.encode_into(&mut encoded);
        encoded.resize(encoded.capacity(), 0);

        let error = PaddedPayloadBody::decode(&encoded, &BODY_SIZE).unwrap_err();
        assert!(matches!(
            error,
            DecodeError::LengthOutOfBounds { len, max, .. }
                if len == usize::from(actual_len) && max == usize::from(BODY_SIZE)
        ));
    }

    #[test]
    fn serde_deserialize_rejects_a_length_past_the_padded_body() {
        #[serde_as]
        #[derive(Serialize)]
        struct InvalidPaddedPayloadBody {
            actual_len: u16,
            #[serde_as(as = "serde_with::Bytes")]
            padded: Box<[u8]>,
        }

        let raw = InvalidPaddedPayloadBody {
            actual_len: BODY_SIZE + 1,
            padded: vec![0; usize::from(BODY_SIZE)].into_boxed_slice(),
        };
        let encoded = bincode::serialize(&raw).unwrap();
        let error = bincode::deserialize::<PaddedPayloadBody>(&encoded).unwrap_err();

        assert!(format!("{error}").contains("Payload too large"));
    }

    #[test]
    fn a_body_takes_content_up_to_its_size() {
        assert!(PaddedPayloadBody::try_new(&[1; BODY_SIZE as usize], BODY_SIZE).is_ok());
        assert!(matches!(
            PaddedPayloadBody::try_new(&[1; BODY_SIZE as usize + 1], BODY_SIZE),
            Err(Error::PayloadTooLarge)
        ));
    }

    #[test]
    fn serde_payload_body_len_keeps_u16_representation() {
        let length = PayloadBodyLen::try_new(1, BODY_SIZE).unwrap();

        assert_eq!(
            bincode::serialize(&length).unwrap(),
            bincode::serialize(&1u16).unwrap()
        );
    }

    #[test]
    fn payload_body_returns_original_unpadded_bytes() {
        let original = b"payload body";
        let body = PaddedPayloadBody::try_new(original.as_slice(), BODY_SIZE).unwrap();
        let payload = Payload::new(PayloadType::Transaction, body);

        assert_eq!(payload.body(), original);

        let (payload_type, body) = payload.into_components();
        assert_eq!(payload_type, PayloadType::Transaction);
        assert_eq!(body, original);
    }
}

/// The bytes a [`Payload`] encodes to besides its body's: its type and the
/// body's content length.
pub const PAYLOAD_HEADER_SIZE: usize = size_of::<PayloadType>() + size_of::<u16>();

/// The exact number of bytes a [`Payload`] whose body is padded to `body_size`
/// encodes to: a fixed enum discriminant, the `u16` body length, and the body.
#[must_use]
pub fn payload_encoded_size(body_size: u16) -> usize {
    PAYLOAD_HEADER_SIZE + usize::from(body_size)
}

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
        self.payload_type
            .encoded_length()
            .checked_add(self.body.encoded_length())
            .unwrap()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.payload_type.encode_into(out);
        self.body.encode_into(out);
    }
}

impl BinaryDecode for Payload {
    /// The size the body is padded to.
    type Context = u16;

    fn decode<'input>(
        input: &'input [u8],
        body_size: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (input, payload_type) = PayloadType::decode(input, &())?;
        let (input, body) = PaddedPayloadBody::decode(input, body_size)?;
        Ok((input, Self { payload_type, body }))
    }
}
