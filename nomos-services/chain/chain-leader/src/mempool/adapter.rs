use std::{marker::PhantomData, pin::Pin};

use futures::Stream;
use nomos_core::{header::HeaderId, mantle::TxHash};
use overwatch::services::relay::OutboundRelay;
use tokio::sync::oneshot;
use tx_service::MempoolMsg;

use super::MempoolAdapter as MempoolAdapterTrait;

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
}
