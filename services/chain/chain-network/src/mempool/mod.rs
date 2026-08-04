use async_trait::async_trait;
use lb_core::mantle::transactions::hash::{TxHash, TxHashPrefix};
use lb_tx_service::TxsWithCommonPrefix;

pub mod adapter;

#[async_trait]
pub trait MempoolAdapter<Tx>: Send + Sync {
    async fn add_transaction(&self, tx: Tx) -> Result<(), overwatch::DynError>;

    async fn remove_transactions(&self, ids: &[TxHash]) -> Result<(), overwatch::DynError>;

    /// The local transactions a single proposal reference could mean.
    ///
    /// A reference is only the leading hash bytes, so several mempool
    /// transactions may answer to it. The stream is unbounded and unordered:
    /// deciding how many candidates are acceptable is the caller's job.
    async fn get_transactions_by_prefix(
        &self,
        prefix: TxHashPrefix,
    ) -> Result<TxsWithCommonPrefix<Tx>, overwatch::DynError>;
}
