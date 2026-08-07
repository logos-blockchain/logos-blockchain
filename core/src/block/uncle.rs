use lb_codec::BinaryCodec;
use lb_cryptarchia_engine::{MAX_UNCLES, Slot, UncleSlots};
use lb_key_management_system_keys::keys::Ed25519Signature;
use lb_utils::bounded::UpperBoundedVec;
use serde::{Deserialize, Serialize};

use crate::header::Header;

/// Signed headers of the uncles referenced by a block.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, BinaryCodec)]
pub struct UncleHeaders(UpperBoundedVec<SignedHeader, MAX_UNCLES>);

impl UncleHeaders {
    #[must_use]
    pub fn new(headers: impl Into<UpperBoundedVec<SignedHeader, MAX_UNCLES>>) -> Self {
        Self(headers.into())
    }

    #[must_use]
    pub const fn empty() -> Self {
        Self(UpperBoundedVec::new_unchecked(Vec::new()))
    }

    /// The slots the carried headers occupy.
    #[must_use]
    pub fn slots(&self) -> UncleSlots {
        let slots: Vec<Slot> = self.0.iter().map(|uncle| uncle.header.slot()).collect();
        slots
            .try_into()
            .expect("one slot per header, so the same bound holds")
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

/// A header together with the signature its leader produced over it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, BinaryCodec)]
pub struct SignedHeader {
    header: Header,
    signature: Ed25519Signature,
}

impl SignedHeader {
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
}
