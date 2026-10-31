use lb_binary_codec::canonical::{BinaryCodec, BinaryDecode, BinaryEncode, DecodeError};
use lb_cryptarchia_engine::UncleSlots;
use lb_key_management_system_keys::keys::Ed25519Signature;
use lb_utils::bounded::{BoundedError, UpperRuntimeBoundedVec};
use serde::{Deserialize, Serialize};

use crate::{
    block::Error,
    header::{Header, HeaderId},
};

/// Signed headers of the uncles referenced by a block, at most as many as the
/// era of the block allows.
///
/// The count is checked against the era's maximum, a `u8` like every era's
/// maximum, when the headers are built or decoded. Serde reads them without
/// an era, under the largest maximum allowed by the type used for length
/// prefix.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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

    #[must_use]
    pub const fn empty() -> Self {
        Self(UpperRuntimeBoundedVec::empty())
    }

    /// The slots the carried headers occupy.
    #[must_use]
    pub fn slots(&self) -> UncleSlots {
        self.0.map_ref(|uncle| uncle.header.slot())
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
