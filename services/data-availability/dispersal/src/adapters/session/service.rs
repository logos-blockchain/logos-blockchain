use broadcast_service::BlockBroadcastService;
use overwatch::services::{ServiceData, relay::OutboundRelay};
use tokio::sync::oneshot;
use tokio_stream::StreamExt as _;

use super::{SessionAdapter, SessionAdapterError, SessionStream};

pub struct BroadcastSessionAdapter<RuntimeServiceId> {
    relay: OutboundRelay<<BlockBroadcastService<RuntimeServiceId> as ServiceData>::Message>,
}

#[async_trait::async_trait]
impl<RuntimeServiceId> SessionAdapter for BroadcastSessionAdapter<RuntimeServiceId>
where
    RuntimeServiceId: Send + Sync + 'static,
{
    type Service = BlockBroadcastService<RuntimeServiceId>;
    fn new(relay: OutboundRelay<<Self::Service as ServiceData>::Message>) -> Self {
        Self { relay }
    }

    async fn subscribe(&self) -> Result<SessionStream, SessionAdapterError> {
        let (sender, receiver) = oneshot::channel();
        self.relay
            .send(broadcast_service::BlockBroadcastMsg::SubscribeDASession {
                result_sender: sender,
            })
            .await
            .map_err(|(e, _)| SessionAdapterError::Channel(e.into()))?;
        let receiver_stream = receiver
            .await
            .map_err(|e| SessionAdapterError::Channel(e.into()))?;

        Ok(Box::pin(
            receiver_stream.map(|update| update.session_number),
        ))
    }
}
