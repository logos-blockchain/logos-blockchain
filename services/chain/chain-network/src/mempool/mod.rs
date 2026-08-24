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
    /// A reference is only the leading hash bytes, so in principle several
    /// mempool transactions could answer to it. The stream is unbounded and
    /// unordered: what a non-unique match means is a consensus question, so it
    /// is the caller's to decide.
    async fn get_transactions_by_prefix(
        &self,
        prefix: TxHashPrefix,
    ) -> Result<TxsWithCommonPrefix<Tx>, overwatch::DynError>;
}
