use core::fmt::{self, Debug, Formatter};

use lb_binary_codec::{bincode::BoundedSerializeOp, canonical::BinaryCodec};
use lb_cryptarchia_engine::Slot;

mod fixtures;
pub mod v1;

use crate::{
    proofs::leader_proof::Groth16LeaderProof,
    utils::{display_hex_bytes_newtype, serde_bytes_newtype},
};

#[derive(Clone, Eq, PartialEq, Copy, Hash, PartialOrd, Ord, BinaryCodec)]
pub struct HeaderId([u8; 32]);

impl HeaderId {
    /// The fixed-size canonical representation of a header identifier.
    pub const CANONICAL_ENCODED_SIZE: usize = 32;
}

impl Debug for HeaderId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "HeaderId({})", hex::encode(self.0))
    }
}

#[derive(Clone, Eq, PartialEq, Copy, Hash, BinaryCodec)]
pub struct ContentId([u8; 32]);

impl ContentId {
    /// The fixed-size canonical representation of a content identifier.
    pub const CANONICAL_ENCODED_SIZE: usize = 32;
}

impl Debug for ContentId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "ContentId({})", hex::encode(self.0))
    }
}

#[derive(Clone, Eq, PartialEq, Copy)]
pub struct Nonce([u8; 32]);

impl Debug for Nonce {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Nonce({})", hex::encode(self.0))
    }
}

/// A header of any version, borrowed from the block or proposal it heads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderRef<'header> {
    V1(&'header v1::Header),
}

impl<'header> HeaderRef<'header> {
    /// The identifier of the header, which names its block.
    #[must_use]
    pub fn id(self) -> HeaderId {
        match self {
            Self::V1(header) => header.id(),
        }
    }

    #[must_use]
    pub const fn slot(self) -> Slot {
        match self {
            Self::V1(header) => header.slot(),
        }
    }

    #[must_use]
    pub const fn parent(self) -> HeaderId {
        match self {
            Self::V1(header) => header.parent(),
        }
    }

    #[must_use]
    pub const fn body_root(self) -> &'header ContentId {
        match self {
            Self::V1(header) => header.body_root(),
        }
    }

    #[must_use]
    pub const fn leader_proof(self) -> &'header Groth16LeaderProof {
        match self {
            Self::V1(header) => header.leader_proof(),
        }
    }
}

impl From<[u8; 32]> for HeaderId {
    fn from(id: [u8; 32]) -> Self {
        Self(id)
    }
}

impl From<HeaderId> for [u8; 32] {
    fn from(id: HeaderId) -> Self {
        id.0
    }
}

impl TryFrom<&[u8]> for HeaderId {
    type Error = Error;

    fn try_from(slice: &[u8]) -> Result<Self, Self::Error> {
        if slice.len() != 32 {
            return Err(Error::InvalidHeaderIdSize(slice.len()));
        }
        let mut id = [0u8; 32];
        id.copy_from_slice(slice);
        Ok(Self::from(id))
    }
}

impl AsRef<[u8]> for HeaderId {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl AsRef<[u8; 32]> for ContentId {
    fn as_ref(&self) -> &[u8; 32] {
        &self.0
    }
}

impl AsRef<[u8; 32]> for Nonce {
    fn as_ref(&self) -> &[u8; 32] {
        &self.0
    }
}

impl From<[u8; 32]> for ContentId {
    fn from(id: [u8; 32]) -> Self {
        Self(id)
    }
}

impl From<ContentId> for [u8; 32] {
    fn from(id: ContentId) -> Self {
        id.0
    }
}

display_hex_bytes_newtype!(HeaderId);
display_hex_bytes_newtype!(ContentId);
display_hex_bytes_newtype!(Nonce);

serde_bytes_newtype!(HeaderId, 32);
serde_bytes_newtype!(ContentId, 32);
serde_bytes_newtype!(Nonce, 32);

impl BoundedSerializeOp for HeaderId {
    type Bytes = [u8; 32];
}

impl BoundedSerializeOp for ContentId {
    type Bytes = [u8; 32];
}

impl BoundedSerializeOp for Nonce {
    type Bytes = [u8; 32];
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invalid header id size: {0}")]
    InvalidHeaderIdSize(usize),
}

#[test]
fn test_serde() {
    use lb_binary_codec::bincode::{DeserializeOp as _, SerializeOp as _};
    let header = HeaderId([0; 32]);
    assert_eq!(
        HeaderId::from_bytes(
            &header
                .to_bytes()
                .expect("HeaderId should be able to be serialized")
        )
        .unwrap(),
        HeaderId([0; 32])
    );
}

#[test]
fn fixed_size_bincode_serialization_matches_for_header_types() {
    use lb_binary_codec::bincode::SerializeOp as _;

    let header_id = HeaderId([0x11; 32]);
    let content_id = ContentId([0x22; 32]);
    let nonce = Nonce([0x33; 32]);

    for (ordinary, bounded) in [
        (
            header_id.to_bytes().unwrap(),
            header_id.to_bounded_bytes().unwrap().to_vec(),
        ),
        (
            content_id.to_bytes().unwrap(),
            content_id.to_bounded_bytes().unwrap().to_vec(),
        ),
        (
            nonce.to_bytes().unwrap(),
            nonce.to_bounded_bytes().unwrap().to_vec(),
        ),
    ] {
        assert_eq!(ordinary.len(), 32);
        assert_eq!(ordinary.as_ref(), bounded.as_slice());
    }
}

#[test]
fn fixed_header_byte_types_borrow_their_stored_bytes() {
    let content_id = ContentId([0x22; 32]);
    let nonce = Nonce([0x33; 32]);

    assert_eq!(content_id.as_ref(), &content_id.0);
    assert_eq!(nonce.as_ref(), &nonce.0);
    assert!(std::ptr::eq(content_id.as_ref(), &raw const content_id.0));
    assert!(std::ptr::eq(nonce.as_ref(), &raw const nonce.0));
}

#[test]
fn test_serde_json_roundtrip() {
    let header = HeaderId([0xAB; 32]);
    let json = serde_json::to_string(&header).unwrap();

    assert_eq!(json, format!("\"{}\"", "ab".repeat(32)));
    assert_eq!(serde_json::from_str::<HeaderId>(&json).unwrap(), header);
}

#[test]
fn test_serde_json_rejects_oversized_hex() {
    let json = format!("\"{}\"", "ab".repeat(33));
    assert!(serde_json::from_str::<HeaderId>(&json).is_err());
}
