use futures::{Stream, StreamExt as _};
use nomos_core::mantle::mock::{MockTransaction, MockTxId};
use nomos_network::{
    NetworkService,
    backends::mock::{Mock, MockBackendMessage, MockContentTopic, MockMessage, NetworkEvent},
    message::NetworkMsg,
};
use overwatch::services::{ServiceData, relay::OutboundRelay};

use crate::network::NetworkAdapter;

pub const MOCK_PUB_SUB_TOPIC: &str = "MockPubSubTopic";
pub const MOCK_CONTENT_TOPIC: &str = "MockContentTopic";
pub const MOCK_TX_CONTENT_TOPIC: MockContentTopic = MockContentTopic::new("Mock", 1, "Tx");

pub struct MockAdapter<RuntimeServiceId> {
    network_relay: OutboundRelay<<NetworkService<Mock, RuntimeServiceId> as ServiceData>::Message>,
}

#[async_trait::async_trait]
impl<RuntimeServiceId> NetworkAdapter<RuntimeServiceId> for MockAdapter<RuntimeServiceId> {
    type Backend = Mock;
    type Settings = ();
    type Payload = MockTransaction<MockMessage>;
    type Key = MockTxId;

    async fn new(
        _settings: Self::Settings,
        network_relay: OutboundRelay<
            <NetworkService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
        >,
    ) -> Self {
        // send message to boot the network producer
        if let Err(e) = network_relay
            .send(NetworkMsg::Process(MockBackendMessage::BootProducer {
                spawner: Box::new(move |fut| {
                    tokio::spawn(fut);
                    Ok(())
                }),
            }))
            .await
        {
            panic!(
                "Couldn't send boot producer message to the network service: {:?}",
                e.0
            );
        }

        if let Err((e, _)) = network_relay
            .send(NetworkMsg::Process(MockBackendMessage::RelaySubscribe {
                topic: MOCK_PUB_SUB_TOPIC.to_owned(),
            }))
            .await
        {
            panic!("Couldn't send subscribe message to the network service: {e}",);
        }
        Self { network_relay }
    }

    async fn payload_stream(
        &self,
    ) -> Box<dyn Stream<Item = (Self::Key, Self::Payload)> + Unpin + Send> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        if let Err((_, e)) = self
            .network_relay
            .send(NetworkMsg::SubscribeToPubSub { sender })
            .await
        {
            tracing::error!(err = ?e);
        }

        let stream = receiver.await.unwrap();
        Box::new(Box::pin(stream.filter_map(async |event| match event {
            Ok(NetworkEvent::RawMessage(message)) => {
                tracing::info!("Received message: {:?}", message.payload());
                message.content_topic().eq(&MOCK_TX_CONTENT_TOPIC).then(|| {
                    let tx = MockTransaction::new(message);
                    (tx.id(), tx)
                })
            }
            Err(_e) => None,
        })))
    }

    async fn send(&self, msg: Self::Payload) {
        if let Err((e, _)) = self
            .network_relay
            .send(NetworkMsg::Process(MockBackendMessage::Broadcast {
                topic: MOCK_PUB_SUB_TOPIC.into(),
                msg: msg.message().clone(),
            }))
            .await
        {
            tracing::error!("failed to send item to topic: {e}");
        }
    }
}
