use lb_binary_codec::canonical::{BinaryCodec, BinaryDecode, BinaryEncode, DecodeError};
use lb_cryptarchia_engine::UncleSlots;
use lb_key_management_system_keys::keys::Ed25519Signature;
use lb_utils::bounded::{BoundedError, RuntimeBoundedVecSeed, UpperRuntimeBoundedVec};
use serde::{Deserialize, Deserializer, Serialize, de::DeserializeSeed};

use crate::{
    block::Error,
    header::{Header, HeaderId},
};

/// Signed headers of the uncles referenced by a block, at most as many as the
/// era of the block allows.
///
/// The headers hold the era's maximum they were built or decoded under, a `u8`
/// like every era's maximum.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct UncleHeaders(UpperRuntimeBoundedVec<SignedHeader, u8>);

impl UncleHeaders {
    /// The largest canonical encoding of the uncles of a block whose era
    /// allows `max_uncles`.
    #[must_use]
    pub fn max_encoded_size(max_uncles: u8) -> usize {
        size_of::<u8>() + usize::from(max_uncles) * SignedHeader::CANONICAL_ENCODED_SIZE
    }

    /// The uncles of a block whose era allows `max_uncles`.
    pub fn try_new(headers: Vec<SignedHeader>, max_uncles: u8) -> Result<Self, BoundedError> {
        UpperRuntimeBoundedVec::try_new(headers, max_uncles).map(Self)
    }

    /// No uncles, in an era allowing `max_uncles`.
    #[must_use]
    pub const fn empty(max_uncles: u8) -> Self {
        Self(UpperRuntimeBoundedVec::empty(max_uncles))
    }

    /// The most uncles the era of the block allows.
    #[must_use]
    pub const fn max_uncles(&self) -> u8 {
        self.0.max_len()
    }

    /// The slots the carried headers occupy, under the same maximum.
    #[must_use]
    pub fn slots(&self) -> UncleSlots {
        UncleSlots::new_unchecked(
            self.0.iter().map(|uncle| uncle.header.slot()).collect(),
            self.0.max_len(),
        )
    }

    pub fn ids(&self) -> impl Iterator<Item = HeaderId> {
        self.0.iter().map(|uncle| uncle.header.id())
    }

    pub fn parents(&self) -> impl Iterator<Item = HeaderId> {
        self.0.iter().map(|uncle| uncle.header.parent())
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &SignedHeader> {
        self.0.iter()
    }
}

/// Deserializes uncle headers under `max_uncles`, refusing a declared count
/// past it before any header is read.
pub(super) struct UncleHeadersSeed {
    max_uncles: u8,
}

impl UncleHeadersSeed {
    pub(super) const fn new(max_uncles: u8) -> Self {
        Self { max_uncles }
    }
}

impl<'de> DeserializeSeed<'de> for UncleHeadersSeed {
    type Value = UncleHeaders;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        RuntimeBoundedVecSeed::new(self.max_uncles)
            .deserialize(deserializer)
            .map(UncleHeaders)
    }
}

// Serde reads blocks without their era (a node's API): the headers are taken
// under the largest maximum their type holds.
impl<'de> Deserialize<'de> for UncleHeaders {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        UncleHeadersSeed::new(u8::MAX).deserialize(deserializer)
    }
}

impl BinaryEncode for UncleHeaders {
    fn encoded_length(&self) -> usize {
        self.0.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.0.encode_into(out);
    }
}

impl BinaryDecode for UncleHeaders {
    /// The most uncles the era of the block allows, which decoding holds the
    /// count to.
    type Context = u8;

    fn decode<'input>(
        input: &'input [u8],
        max_uncles: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (rest, headers) = UpperRuntimeBoundedVec::decode(input, &(*max_uncles, ()))?;
        Ok((rest, Self(headers)))
    }
}

/// A header together with the signature its leader produced over it.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize, BinaryCodec)]
pub struct SignedHeader {
    header: Header,
    signature: Ed25519Signature,
}

impl SignedHeader {
    /// The fixed-size canonical representation of a signed header.
    pub const CANONICAL_ENCODED_SIZE: usize =
        Header::CANONICAL_ENCODED_SIZE + Ed25519Signature::CANONICAL_ENCODED_SIZE;

    #[must_use]
    pub const fn new(header: Header, signature: Ed25519Signature) -> Self {
        Self { header, signature }
    }

    #[must_use]
    pub const fn header(&self) -> &Header {
        &self.header
    }

    #[must_use]
    pub const fn signature(&self) -> &Ed25519Signature {
        &self.signature
    }

    pub fn verify(&self) -> Result<(), Error> {
        crate::block::verify_header_alone(&self.header)?;
        crate::block::verify_header_signature(&self.header, &self.signature)
    }
}
