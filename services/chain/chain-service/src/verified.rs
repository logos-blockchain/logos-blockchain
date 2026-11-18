//! Block Header Verification, and the types of what passed it.

use lb_binary_codec::canonical::BinaryDecode;
use lb_core::{
    block::{Block, BlockTransactions, UncleHeaders, verify_header_alone},
    header::{Header, HeaderId},
    mantle::{
        TxHash,
        traits::{Hashable, StorageSize},
    },
    proofs::leader_proof::LeaderProof as _,
};
use lb_cryptarchia_engine::Slot;
use lb_ledger::LedgerError;
use lb_storage_service::api::StorageApi;

use crate::{Cryptarchia, Error};

/// A header that passed Block Header Verification, with the uncles it was
/// verified with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedBlockHeader {
    header: Header,
    uncle_headers: UncleHeaders,
}

impl VerifiedBlockHeader {
    /// Verifies `header` with `uncle_headers` against the block tree and the
    /// ledger of `cryptarchia` at `current_slot`.
    ///
    /// This checks the rules 5~10 of [Block Header Validation](https://lip.logos.co/blockchain/raw/cryptarchia-v1-protocol.html#block-header-validation),
    /// as well as the rules that need the header alone.
    /// Rule 8 (Block is a descendant of LIB) is implied by Rule 7, since all
    /// forks deeper than LIB are pruned.
    pub(crate) fn new(
        header: Header,
        uncle_headers: UncleHeaders,
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

        let public_inputs = cryptarchia
            .ledger
            .leader_public(header.parent(), header.slot())
            .map_err(|err| match err {
                LedgerError::ParentNotFound(parent) => Error::ParentMissing {
                    parent,
                    info: Box::new(cryptarchia.info()),
                },
                LedgerError::InvalidSlot { parent, block } => Error::InvalidSlot { parent, block },
                err => Error::Ledger(err),
            })?;
        // The proof is bound to the block ID of `header`.
        if !header.verify(&public_inputs) {
            return Err(Error::InvalidLeaderProof);
        }

        cryptarchia.verify_uncles(&header, &uncle_headers)?;

        Ok(Self {
            header,
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

    #[must_use]
    pub const fn header(&self) -> &Header {
        self.0.header()
    }

    #[must_use]
    pub const fn uncle_headers(&self) -> &UncleHeaders {
        self.0.uncle_headers()
    }

    pub fn transactions_iter(&self) -> impl ExactSizeIterator<Item = &Tx> + '_ {
        self.0.transactions_iter()
    }
}

impl<Tx> From<BlockWithVerifiedHeader<Tx>> for Block<Tx> {
    fn from(block: BlockWithVerifiedHeader<Tx>) -> Self {
        block.0
    }
}
