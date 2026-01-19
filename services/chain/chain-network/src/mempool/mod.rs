use async_trait::async_trait;
use lb_core::{header::HeaderId, mantle::TxHash};
use lb_tx_service::TransactionsByHashesResponse;

pub mod adapter;

#[async_trait]
pub trait MempoolAdapter<Tx>: Send + Sync {
    async fn mark_transactions_in_block(
        &self,
        ids: &[TxHash],
        block: HeaderId,
    ) -> Result<(), overwatch::DynError>;

    async fn get_transactions_by_hashes(
        &self,
        hashes: Vec<TxHash>,
    ) -> Result<TransactionsByHashesResponse<Tx, TxHash>, overwatch::DynError>;
}
