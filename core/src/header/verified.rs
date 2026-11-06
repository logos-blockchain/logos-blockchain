//! Block Header Verification

use lb_key_management_system_keys::keys::Ed25519Signature;
use lb_time::Slot;

use crate::{
    block::{HeaderError, UncleHeaders, verify_header_alone, verify_header_signature},
    header::{Header, HeaderId},
    proofs::leader_proof::{Groth16LeaderProof, LeaderProof as _, LeaderPublic},
};

/// A header that passed Block Header Verification, with the uncles it was
/// verified with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedBlockHeader {
    header: Header,
    signature: Ed25519Signature,
    uncle_headers: UncleHeaders,
}

impl VerifiedBlockHeader {
    /// Verifies [`Header`] with `uncle_headers` and `signature` against
    /// `context`.
    ///
    /// This checks the rule 5~10 defined in [Block Header Verification](https://lip.logos.co/blockchain/raw/cryptarchia-v1-protocol.html#block-header-validation).
    /// 5. Block's header comes after the parent's slot.
    /// 6. Block's slot time is not after the current slot time.
    /// 7. We have already accepted the parent block.
    /// 8. Block is a decendant of LIB (This is implied by Rule 7 if all forks
    ///    deeper than LIB are pruned).
    /// 9. `PoL` is valid
    /// 10. Uncle headers are valid.
    pub fn new(
        header: Header,
        uncle_headers: UncleHeaders,
        signature: Ed25519Signature,
        context: &impl HeaderVerificationContext,
    ) -> Result<Self, Error> {
        verify_header_alone(&header)?;

        if !context.has_block(&header.parent()) {
            return Err(Error::ParentMissing(header.parent()));
        }

        let current_slot = context.current_slot();
        if header.slot() > current_slot {
            return Err(Error::FutureBlock {
                block_slot: header.slot(),
                current_slot,
            });
        }

        verify_header_signature(&header, &signature).map_err(|_| Error::InvalidSignature)?;

        if !header
            .leader_proof()
            .verify(&context.leader_public(header.parent(), header.slot())?)
        {
            return Err(Error::InvalidLeaderProof);
        }

        context.verify_uncles(&header, &uncle_headers)?;

        Ok(Self {
            header,
            signature,
            uncle_headers,
        })
    }

    /// Takes a header that this node verified before, without verifying it
    /// again. Only [`BlockWithVerifiedHeader::from_trusted`] uses it.
    ///
    /// [`BlockWithVerifiedHeader::from_trusted`]: crate::block::BlockWithVerifiedHeader::from_trusted
    #[must_use]
    pub(crate) const fn from_trusted(
        header: Header,
        signature: Ed25519Signature,
        uncle_headers: UncleHeaders,
    ) -> Self {
        Self {
            header,
            signature,
            uncle_headers,
        }
    }

    /// Takes `header` as verified without verifying it, for tests that need a
    /// [`VerifiedBlockHeader`] without a valid proof of leadership.
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub const fn new_unchecked(
        header: Header,
        signature: Ed25519Signature,
        uncle_headers: UncleHeaders,
    ) -> Self {
        Self::from_trusted(header, signature, uncle_headers)
    }

    #[must_use]
    pub const fn header(&self) -> &Header {
        &self.header
    }

    #[must_use]
    pub const fn uncle_headers(&self) -> &UncleHeaders {
        &self.uncle_headers
    }

    #[must_use]
    pub const fn signature(&self) -> &Ed25519Signature {
        &self.signature
    }

    #[must_use]
    pub const fn leader_proof(&self) -> &Groth16LeaderProof {
        self.header.leader_proof()
    }

    pub(crate) fn into_parts(self) -> (Header, Ed25519Signature, UncleHeaders) {
        (self.header, self.signature, self.uncle_headers)
    }
}

/// What Block Header Verification needs to know about the chain.
pub trait HeaderVerificationContext {
    fn current_slot(&self) -> Slot;

    fn has_block(&self, id: &HeaderId) -> bool;

    /// The public inputs that a `PoL` for `slot` must verify against,
    /// built from the state of `parent`.
    fn leader_public(&self, parent: HeaderId, slot: Slot) -> Result<LeaderPublic, Error>;

    /// Verifies the uncles that `header` carries.
    // TODO: remove this once the uncle rules in chain-service move to this crate.
    fn verify_uncles(&self, header: &Header, uncle_headers: &UncleHeaders) -> Result<(), Error>;
}

#[derive(Debug, thiserror::Error)]
// TODO: embed error types in other modules by refactoring them into smaller
// error types.
pub enum Error {
    #[error(transparent)]
    Header(#[from] HeaderError),
    #[error("parent {0:?} is missing")]
    ParentMissing(HeaderId),
    #[error("block slot {block_slot:?} is after the current slot {current_slot:?}")]
    FutureBlock {
        block_slot: Slot,
        current_slot: Slot,
    },
    #[error("invalid header signature")]
    InvalidSignature,
    #[error("Invalid block slot {block:?} for parent slot {parent:?}")]
    InvalidSlot { parent: Slot, block: Slot },
    #[error("invalid proof of leadership")]
    InvalidLeaderProof,
    /// The uncles carried with the header break a rule of Uncle Validation.
    #[error("invalid uncles: {0}")]
    InvalidUncles(String),
    #[error("other error: {0}")]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}
