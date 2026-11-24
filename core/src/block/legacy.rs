//! The encoding a block has outside the node: in storage and in chain sync.
//!
//! A block is encoded canonically, but for the blocks of a chain's legacy era,
//! which keep the encoding the releases that produced them stored and served
//! them in: the configured bincode of the block's serde form, in which each
//! transaction is a byte string holding its canonical encoding in the legacy
//! layout. Both encodings open with the block's slot, which tells them apart.

use lb_binary_codec::{
    bincode::{DeserializeOp as _, SerializeOp as _},
    canonical::{BinaryDecode, BinaryEncode, DecodeError},
};
use lb_cryptarchia_engine::Slot;
use lb_key_management_system_keys::keys::Ed25519Signature;
use serde::{Deserialize, Serialize};

use crate::{
    block::{Block, BlockLimits, BlockTransactions, SignedHeader, UncleHeaders},
    header::Header,
    mantle::{
        TxHash,
        traits::{Hashable, StorageSize},
        transactions::{TxDecodeContext, TxFormat},
    },
};

/// A block as releases 0.3.1 to 0.3.6 serialize it.
#[derive(Serialize, Deserialize)]
struct LegacyBlock {
    header: Header,
    signature: Ed25519Signature,
    uncle_headers: Vec<SignedHeader>,
    transactions: Vec<Vec<u8>>,
}

#[expect(
    clippy::multiple_inherent_impl,
    reason = "The encodings a block has outside the node are kept apart from the block itself."
)]
impl<Tx> Block<Tx> {
    /// The block in the encoding of the era of its slot.
    #[must_use]
    pub fn encode_wire(&self) -> Vec<u8>
    where
        Tx: BinaryEncode + Hashable<Hash = TxHash> + StorageSize,
    {
        match TxFormat::at_slot(self.header().slot()) {
            TxFormat::ForkBound => self.encode_to_vec(),
            TxFormat::Legacy => LegacyBlock {
                header: self.header().clone(),
                signature: *self.signature(),
                uncle_headers: self.uncle_headers().iter().cloned().collect(),
                transactions: self
                    .transactions_iter()
                    .map(BinaryEncode::encode_to_vec)
                    .collect(),
            }
            .to_bytes()
            .expect("A block serializes to bytes.")
            .into(),
        }
    }

    /// Decodes a block from the encoding of the era of its slot, which it
    /// opens with, under the `limits` of that era and the most bytes of data
    /// it allows an Operation.
    pub fn decode_wire(
        bytes: &[u8],
        limits: BlockLimits,
        max_op_data_size: u32,
    ) -> Result<Self, DecodeError>
    where
        Tx: BinaryDecode<Context = TxDecodeContext> + Hashable<Hash = TxHash> + StorageSize,
    {
        let BlockLimits {
            max_uncles,
            max_transactions,
            max_transactions_size,
        } = limits;
        let slot = Slot::peek_decode(bytes, &())?;
        let tx_context = TxDecodeContext::at_slot(slot, max_op_data_size);
        if !tx_context.format.is_legacy() {
            return Self::decode_all(
                bytes,
                &(
                    max_uncles,
                    max_transactions_size,
                    (max_transactions, tx_context),
                ),
            );
        }

        let LegacyBlock {
            header,
            signature,
            uncle_headers,
            transactions,
        } = LegacyBlock::from_bytes(bytes)
            .map_err(|error| DecodeError::custom(error.to_string()))?;
        let uncle_headers = UncleHeaders::try_new(uncle_headers, max_uncles)
            .map_err(|error| DecodeError::custom(error.to_string()))?;
        let transactions = transactions
            .iter()
            .map(|transaction| Tx::decode_all(transaction, &tx_context))
            .collect::<Result<Vec<_>, _>>()?;
        let transactions = BlockTransactions::try_new(transactions, max_transactions)
            .map_err(|error| DecodeError::custom(error.to_string()))?;
        Self::validate_total_transactions_size(&transactions, max_transactions_size)
            .map_err(|error| DecodeError::invalid_value::<Self>(error.to_string()))?;
        Ok(Self::reconstruct(
            header,
            uncle_headers,
            transactions,
            signature,
        ))
    }
}
