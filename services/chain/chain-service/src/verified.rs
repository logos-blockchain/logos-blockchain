//! Block Header Verification, and the types of what passed it.

use std::ops::Deref;

use lb_binary_codec::canonical::BinaryDecode;
use lb_core::{
    block::{Block, BlockTransactions, UncleHeaders, verify_header_alone, verify_header_signature},
    header::{Header, HeaderId},
    mantle::{
        TxHash,
        traits::{Hashable, StorageSize},
    },
    proofs::leader_proof::LeaderProof as _,
};
use lb_cryptarchia_engine::Slot;
use lb_key_management_system_keys::keys::Ed25519Signature;
use lb_ledger::LedgerError;
use lb_storage_service::api::StorageApi;

use crate::{Cryptarchia, Error};

/// A header that passed Block Header Verification, with the uncles and the
/// signature it was verified with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedBlockHeader {
    header: Header,
    signature: Ed25519Signature,
    uncle_headers: UncleHeaders,
}

impl VerifiedBlockHeader {
    /// Verifies `header` with `uncle_headers` and `signature` against the
    /// block tree and the ledger of `cryptarchia` at `current_slot`.
    ///
    /// This checks the rules 5~10 of [Block Header Validation](https://lip.logos.co/blockchain/raw/cryptarchia-v1-protocol.html#block-header-validation),
    /// as well as the rules that need the header alone, and the signature.
    /// Rule 8 (Block is a descendant of LIB) is implied by Rule 7, since all
    /// forks deeper than LIB are pruned.
    pub(crate) fn new(
        header: Header,
        uncle_headers: UncleHeaders,
        signature: Ed25519Signature,
        cryptarchia: &Cryptarchia,
        current_slot: Slot,
    ) -> Result<Self, Error> {
        verify_header_alone(&header).map_err(Error::InvalidHeader)?;

        if cryptarchia
            .consensus
            .branches()
            .get(&header.parent())
            .is_none()
        {
            return Err(Error::ParentMissing {
                parent: header.parent(),
                info: Box::new(cryptarchia.info()),
            });
        }

        if header.slot() > current_slot {
            return Err(Error::FutureBlock {
                block_slot: header.slot(),
                current_slot,
            });
        }

        verify_header_signature(&header, &signature).map_err(|_| Error::InvalidSignature)?;

        let public_inputs = cryptarchia
            .ledger
            .leader_public(header.parent(), header.slot())
            .map_err(|err| match err {
                LedgerError::ParentNotFound(parent) => Error::ParentMissing {
                    parent,
                    info: Box::new(cryptarchia.info()),
                },
                err => Error::Ledger(err),
            })?;
        if !header.leader_proof().verify(&public_inputs) {
            return Err(Error::InvalidLeaderProof);
        }

        cryptarchia.verify_uncles(&header, &uncle_headers)?;

        Ok(Self {
            header,
            signature,
            uncle_headers,
        })
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
}

/// A [`Block`] whose header passed Block Header Verification.
///
/// Its transactions are not verified yet. They are verified against the ledger
/// while the block is applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockWithVerifiedHeader<Tx>(Block<Tx>);

impl<Tx> BlockWithVerifiedHeader<Tx> {
    /// Assembles a block from `header` and `transactions`.
    ///
    /// # Errors
    ///
    /// Returns an error if the body root of `header` does not commit to its
    /// uncles and `transactions`. Whether `transactions` fit in the block's era
    /// is checked by the ledger when the block is applied.
    pub fn new(
        header: VerifiedBlockHeader,
        transactions: BlockTransactions<Tx>,
    ) -> Result<Self, lb_core::block::Error>
    where
        Tx: Hashable<Hash = TxHash> + StorageSize,
    {
        Block::validate_body_root(&header.header, &header.uncle_headers, &transactions)?;
        Ok(Self(Block::reconstruct(
            header.header,
            header.uncle_headers,
            transactions,
            header.signature,
        )))
    }

    /// Reads the block `id` from `storage`, without verifying it, since this
    /// node verified the block before storing it.
    pub(crate) async fn load_stored(storage: &StorageApi<Tx>, id: &HeaderId) -> Option<Self>
    where
        Tx: BinaryDecode<Context = u32> + Hashable<Hash = TxHash> + StorageSize,
    {
        storage.get_block(id).await.map(Self)
    }

    /// Takes `block` as verified without verifying it, for tests that need a
    /// [`BlockWithVerifiedHeader`] regardless of the chain.
    #[cfg(test)]
    pub(crate) const fn new_unchecked(block: Block<Tx>) -> Self {
        Self(block)
    }
}

impl<Tx> Deref for BlockWithVerifiedHeader<Tx> {
    type Target = Block<Tx>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<Tx> From<BlockWithVerifiedHeader<Tx>> for Block<Tx> {
    fn from(block: BlockWithVerifiedHeader<Tx>) -> Self {
        block.0
    }
}
