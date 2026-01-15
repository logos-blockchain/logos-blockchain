use async_trait::async_trait;
use nomos_core::mantle::TxHash;
use tx_service::TransactionsByHashesResponse;

pub mod adapter;

#[async_trait]
pub trait MempoolAdapter<Tx>: Send + Sync {
    async fn remove_transactions(&self, ids: &[TxHash]) -> Result<(), overwatch::DynError>;

    async fn get_transactions_by_hashes(
        &self,
        hashes: Vec<TxHash>,
    ) -> Result<TransactionsByHashesResponse<Tx, TxHash>, overwatch::DynError>;
}
