use std::collections::HashMap;

use async_trait::async_trait;
use lb_core::{
    header::HeaderId,
    mantle::transactions::hash::{TxHash, TxHashPrefix},
};
use lb_cryptarchia_engine::Slot;
use lb_tx_service::TxsWithCommonPrefix;

pub mod adapter;

#[async_trait]
pub trait MempoolAdapter<Tx>: Send + Sync {
    async fn add_transaction(&self, tx: Tx) -> Result<(), overwatch::DynError>;

    /// Retire the transactions carried by the blocks that entered the
    /// canonical chain, each mapped to the slot of the block carrying it.
    async fn retire_entered_canonical_transactions(
        &self,
        txs: HashMap<TxHash, Slot>,
    ) -> Result<(), overwatch::DynError>;

    /// Tell the mempool that `block_id`, at `slot`, has been processed, and
    /// that the latest immutable block is at `lib_slot`. The mempool has no
    /// other notion of chain time.
    async fn notify_processed_block(
        &self,
        block_id: HeaderId,
        slot: Slot,
        lib_slot: Slot,
    ) -> Result<(), overwatch::DynError>;

    /// The local transactions a single proposal reference could mean.
    ///
    /// A reference is only the leading hash bytes, so in principle several
    /// mempool transactions could answer to it. The stream is unbounded and
    /// unordered: what a non-unique match means is a consensus question, so it
    /// is the caller's to decide.
    async fn get_transactions_by_prefix(
        &self,
        prefix: TxHashPrefix,
    ) -> Result<TxsWithCommonPrefix<Tx>, overwatch::DynError>;
}
