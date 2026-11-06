//! What Block Header Verification reads from the block tree and the ledger.

use lb_core::{
    block::UncleHeaders,
    header::{Header, HeaderId, HeaderVerificationError},
    proofs::leader_proof::LeaderPublic,
};
use lb_cryptarchia_engine::Slot;
use lb_ledger::LedgerError;

use crate::{Cryptarchia, Error};

/// The block tree and the ledger of a [`Cryptarchia`], at `current_slot`.
pub struct HeaderVerificationContext<'c> {
    cryptarchia: &'c Cryptarchia,
    current_slot: Slot,
}

impl<'c> HeaderVerificationContext<'c> {
    pub const fn new(cryptarchia: &'c Cryptarchia, current_slot: Slot) -> Self {
        Self {
            cryptarchia,
            current_slot,
        }
    }
}

impl lb_core::header::HeaderVerificationContext for HeaderVerificationContext<'_> {
    fn current_slot(&self) -> Slot {
        self.current_slot
    }

    fn has_block(&self, id: &HeaderId) -> bool {
        self.cryptarchia.consensus.branches().get(id).is_some()
    }

    fn leader_public(
        &self,
        parent: HeaderId,
        slot: Slot,
    ) -> Result<LeaderPublic, HeaderVerificationError> {
        self.cryptarchia
            .ledger
            .leader_public(parent, slot)
            .map_err(|e| match e {
                LedgerError::ParentNotFound(parent) => {
                    HeaderVerificationError::ParentMissing(parent)
                }
                LedgerError::InvalidSlot { parent, block } => {
                    HeaderVerificationError::InvalidSlot { parent, block }
                }
                e => HeaderVerificationError::Other(e.into()),
            })
    }

    fn verify_uncles(
        &self,
        header: &Header,
        uncle_headers: &UncleHeaders,
    ) -> Result<(), HeaderVerificationError> {
        self.cryptarchia
            .verify_uncles(header, uncle_headers)
            .map_err(|e| match e {
                Error::InvalidUncle { uncle, reason } => HeaderVerificationError::InvalidUncle {
                    uncle,
                    reason: reason.to_string(),
                },
                e => HeaderVerificationError::Other(e.into()),
            })
    }
}
