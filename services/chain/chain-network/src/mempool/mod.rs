use std::collections::HashMap;

use async_trait::async_trait;
use lb_core::{
    header::HeaderId,
    mantle::transactions::hash::{TxHash, TxHashPrefix},
};
use lb_cryptarchia_engine::{Era, Slot};
use lb_tx_service::TxsWithCommonPrefix;

pub mod adapter;

#[async_trait]
pub trait MempoolAdapter<Tx>: Send + Sync {
    async fn rotate_era(&mut self, era: Era);
    async fn complete_era_transition(&mut self, ended_era: Era);

    /// Tell the mempool that the canonical chain was updated, either by growing
    /// with a new block or by a reorg.
    ///
    /// - `entered`: txs carried by the blocks that entered the canonical chain,
    ///   each mapped to the slot of the block carrying it.
    /// - `reorged`: txs carried by the blocks that left the canonical chain and
    ///   not by the blocks now in it, which the mempool admits again. Empty
    ///   when the chain only grows.
    async fn notify_canonical_chain_updated(
        &self,
        entered: HashMap<TxHash, Slot>,
        reorged: Vec<Tx>,
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
