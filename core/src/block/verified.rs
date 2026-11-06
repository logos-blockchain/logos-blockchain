//! A block whose header passed Block Header Verification.

use crate::{
    block::{Block, BlockTransactions, Error},
    header::{HeaderVerificationError, VerifiedBlockHeader},
    mantle::{
        traits::{Hashable, StorageSize},
        transactions::hash::TxHash,
    },
};

/// A block whose header is a [`VerifiedBlockHeader`], with the transactions
/// that header commits to.
///
/// Its transactions are not verified yet. They are verified against the ledger
/// while the block is applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockWithVerifiedHeader<Tx> {
    header: VerifiedBlockHeader,
    transactions: BlockTransactions<Tx>,
}

impl<Tx> BlockWithVerifiedHeader<Tx> {
    /// Binds `transactions` to `header`.
    ///
    /// # Errors
    ///
    /// Returns an error if the body root of `header` does not commit to its
    /// uncles and `transactions`, or if `transactions` do not fit in a block.
    pub fn new(
        header: VerifiedBlockHeader,
        transactions: BlockTransactions<Tx>,
    ) -> Result<Self, Error>
    where
        Tx: Hashable<Hash = TxHash> + StorageSize,
    {
        Block::validate_total_transactions_size(&transactions)?;
        Block::validate_body_root(header.header(), header.uncle_headers(), &transactions)?;
        Ok(Self {
            header,
            transactions,
        })
    }

    /// Takes a block read back from this node's storage, which this node
    /// verified before storing it, without verifying it again.
    // TODO: find a safer way that doesn't expose this as public
    #[must_use]
    pub fn from_trusted(block: Block<Tx>) -> Self {
        let Block {
            header,
            signature,
            uncle_headers,
            transactions,
        } = block;
        Self {
            header: VerifiedBlockHeader::from_trusted(header, signature, uncle_headers),
            transactions,
        }
    }

    #[must_use]
    pub const fn header(&self) -> &VerifiedBlockHeader {
        &self.header
    }

    #[must_use]
    pub const fn transactions(&self) -> &BlockTransactions<Tx> {
        &self.transactions
    }

    pub fn transactions_iter(&self) -> impl ExactSizeIterator<Item = &Tx> + '_ {
        self.transactions.as_slice().iter()
    }
}

impl<Tx> From<BlockWithVerifiedHeader<Tx>> for Block<Tx> {
    fn from(block: BlockWithVerifiedHeader<Tx>) -> Self {
        let (header, signature, uncle_headers) = block.header.into_parts();
        Self {
            header,
            signature,
            uncle_headers,
            transactions: block.transactions,
        }
    }
}

/// Why a [`Block`] cannot become a [`BlockWithVerifiedHeader`].
#[derive(Debug, thiserror::Error)]
pub enum BlockVerificationError {
    #[error(transparent)]
    Header(#[from] HeaderVerificationError),
    #[error(transparent)]
    Body(#[from] Error),
}
