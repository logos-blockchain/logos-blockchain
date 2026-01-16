use std::marker::PhantomData;

use logos_blockchain_core::{header::HeaderId, mantle::TxHash};
use overwatch::services::relay::OutboundRelay;
use tokio::sync::oneshot;
use logos_blockchain_tx_service::{MempoolMsg, TransactionsByHashesResponse};

use super::MempoolAdapter as MempoolAdapterTrait;

#[derive(Clone)]
pub struct MempoolAdapter<Payload, Tx> {
    mempool_relay: OutboundRelay<MempoolMsg<HeaderId, Payload, Tx, TxHash>>,
    _payload: PhantomData<Payload>,
}

impl<Payload, Tx> MempoolAdapter<Payload, Tx> {
    #[must_use]
    pub const fn new(
        mempool_relay: OutboundRelay<MempoolMsg<HeaderId, Payload, Tx, TxHash>>,
    ) -> Self {
        Self {
            mempool_relay,
            _payload: PhantomData,
        }
    }
}

#[async_trait::async_trait]
impl<Payload, Tx> MempoolAdapterTrait<Tx> for MempoolAdapter<Payload, Tx>
where
    Payload: Send + Sync,
    Tx: Send + Sync + 'static,
{
    async fn mark_transactions_in_block(
        &self,
        ids: &[TxHash],
        block: HeaderId,
    ) -> Result<(), overwatch::DynError> {
        self.mempool_relay
            .send(MempoolMsg::MarkInBlock {
                ids: ids.to_vec(),
                block,
            })
            .await
            .map_err(|(e, _)| format!("Could not mark transactions in block: {e}"))?;

        Ok(())
    }

    async fn get_transactions_by_hashes(
        &self,
        hashes: Vec<TxHash>,
    ) -> Result<TransactionsByHashesResponse<Tx, TxHash>, overwatch::DynError> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.mempool_relay
            .send(MempoolMsg::GetTransactionsByHashes {
                hashes,
                reply_channel: resp_tx,
            })
            .await
            .map_err(|(e, _)| format!("Could not get transactions by hashes: {e}"))?;

        let response = resp_rx
            .await
            .map_err(|e| format!("Could not receive response: {e}"))?;

        Ok(response.map_err(|e| format!("Mempool error: {e}"))?)
    }
}
