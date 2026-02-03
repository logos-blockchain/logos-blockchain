use std::pin::Pin;

use futures::Stream;
use lb_core::{
    header::HeaderId,
    mantle::{Transaction, TxHash},
};
use lb_tx_service::MempoolMsg;
use overwatch::services::relay::OutboundRelay;
use tokio::sync::oneshot;

use super::MempoolAdapter as MempoolAdapterTrait;

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
    Tx: Transaction<Hash = TxHash> + Send + Sync + 'static,
{
    async fn get_mempool_view(
        &self,
        ancestor_hint: HeaderId,
    ) -> Result<Pin<Box<dyn Stream<Item = Tx> + Send>>, overwatch::DynError> {
        let (reply_channel, receiver) = oneshot::channel();

        self.mempool_relay
            .send(MempoolMsg::View {
                ancestor_hint,
                reply_channel,
            })
            .await
            .map_err(|(e, _)| format!("Could not get mempool view: {e}"))?;

        let view_stream = receiver
            .await
            .map_err(|e| overwatch::DynError::from(format!("Failed to get mempool view: {e}")))?;

        Ok(view_stream)
    }

    async fn remove_transactions(&self, ids: &[TxHash]) -> Result<(), overwatch::DynError> {
        self.mempool_relay
            .send(MempoolMsg::Remove { ids: ids.to_vec() })
            .await
            .map_err(|(e, _)| format!("Could not remove transactions from mempool: {e}"))?;

        Ok(())
    }

    async fn post_tx(&self, tx: Tx) -> Result<(), overwatch::DynError> {
        let (reply_channel, receiver) = oneshot::channel();
        self.mempool_relay
            .send(MempoolMsg::Add {
                key: tx.hash(),
                payload: tx,
                reply_channel,
            })
            .await
            .map_err(|(e, _)| format!("Failed to send MempoolMsg::Add: {e}"))?;

        receiver
            .await?
            .map_err(|e| format!("Failed to post transaction to mempool: {e}").into())
    }
}
