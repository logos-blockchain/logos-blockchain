use std::collections::HashMap;

use lb_core::{
    header::HeaderId,
    mantle::transactions::hash::{TxHash, TxHashPrefix},
};
use lb_cryptarchia_engine::{Era, Slot};
use lb_tx_service::{MempoolMsg, TxsWithCommonPrefix};
use overwatch::services::relay::OutboundRelay;
use tokio::sync::oneshot;

use super::MempoolAdapter as MempoolAdapterTrait;

#[derive(Clone)]
pub struct MempoolAdapter<Tx> {
    mempool_relay: OutboundRelay<MempoolMsg<HeaderId, Tx, Tx, TxHash>>,
}

impl<Tx> MempoolAdapter<Tx> {
    #[must_use]
    pub const fn new(mempool_relay: OutboundRelay<MempoolMsg<HeaderId, Tx, Tx, TxHash>>) -> Self {
        Self { mempool_relay }
    }
}

#[async_trait::async_trait]
impl<Tx> MempoolAdapterTrait<Tx> for MempoolAdapter<Tx>
where
    Tx: Send + 'static,
{
    async fn rotate_era(&mut self, _era: Era) {}

    async fn complete_era_transition(&mut self, _ended_era: Era) {}

    async fn notify_canonical_chain_updated(
        &self,
        entered: HashMap<TxHash, Slot>,
        reorged: Vec<Tx>,
    ) -> Result<(), overwatch::DynError> {
        self.mempool_relay
            .send(MempoolMsg::CanonicalChainUpdated { entered, reorged })
            .await
            .map_err(|error| {
                format!("Could not notify mempool of canonical chain update: {error}")
            })?;

        Ok(())
    }

    async fn notify_processed_block(
        &self,
        block_id: HeaderId,
        slot: Slot,
        lib_slot: Slot,
    ) -> Result<(), overwatch::DynError> {
        self.mempool_relay
            .send(MempoolMsg::ProcessedBlock {
                block_id,
                slot,
                lib_slot,
            })
            .await
            .map_err(|error| format!("Could not notify mempool of processed block: {error}"))?;

        Ok(())
    }

    async fn get_transactions_by_prefix(
        &self,
        prefix: TxHashPrefix,
    ) -> Result<TxsWithCommonPrefix<Tx>, overwatch::DynError> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.mempool_relay
            .send(MempoolMsg::GetTransactionsByPrefix {
                prefix,
                reply_channel: resp_tx,
            })
            .await
            .map_err(|error| format!("Could not get transactions by prefix: {error}"))?;

        let response = resp_rx
            .await
            .map_err(|e| format!("Could not receive response: {e}"))?;

        Ok(response.map_err(|e| format!("Mempool error: {e}"))?)
    }
}
