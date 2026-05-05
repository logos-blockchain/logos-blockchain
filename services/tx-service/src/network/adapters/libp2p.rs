use futures::Stream;
use lb_core::codec::{DeserializeOp as _, SerializeOp as _};
use lb_network_service::{
    NetworkService,
    backends::libp2p::{Command, Libp2p, Message, PubSubCommand, TopicHash},
    message::NetworkMsg,
};
use overwatch::services::{ServiceData, relay::OutboundRelay};
use serde::{Serialize, de::DeserializeOwned};
use tokio_stream::StreamExt as _;

use crate::network::NetworkAdapter;

pub struct Libp2pAdapter<Item, Key, RuntimeServiceId> {
    network_relay:
        OutboundRelay<<NetworkService<Libp2p, RuntimeServiceId> as ServiceData>::Message>,
    settings: Settings<Key, Item>,
}

#[async_trait::async_trait]
impl<Item, Key, RuntimeServiceId> NetworkAdapter<RuntimeServiceId>
    for Libp2pAdapter<Item, Key, RuntimeServiceId>
where
    Item: DeserializeOwned + Serialize + Send + Sync + 'static + Clone,
    Key: Clone + Send + Sync + 'static,
{
    type Backend = Libp2p;
    type Settings = Settings<Key, Item>;
    type Payload = Item;
    type Key = Key;

    async fn new(
        settings: Self::Settings,
        network_relay: OutboundRelay<
            <NetworkService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
        >,
    ) -> Self {
        tracing::debug!("Subscribing tx adapter to pubsub topic {}", settings.topic);
        network_relay
            .send(NetworkMsg::Process(Command::PubSub(
                PubSubCommand::Subscribe(settings.topic.clone()),
            )))
            .await
            .expect("Network backend should be ready");
        Self {
            network_relay,
            settings,
        }
    }
    async fn payload_stream(
        &self,
    ) -> Box<dyn Stream<Item = (Self::Key, Self::Payload)> + Unpin + Send> {
        let topic_hash = TopicHash::from_raw(self.settings.topic.clone());
        let id = self.settings.id;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.network_relay
            .send(NetworkMsg::SubscribeToPubSub { sender })
            .await
            .expect("Network backend should be ready");

        let stream = receiver.await.unwrap();
        Box::new(Box::pin(stream.filter_map(move |message| match message {
            Ok(Message { data, topic, .. }) if topic == topic_hash => {
                match Item::from_bytes(&data) {
                    Ok(item) => Some((id(&item), item)),
                    Err(e) => {
                        tracing::debug!("Unrecognized message: {e}");
                        None
                    }
                }
            }
            _ => None,
        })))
    }

    async fn send(&self, item: Item) {
        let serialized = item
            .to_bytes()
            .expect("Item should be able to be serialized");
        {
            if let Err((e, _)) = self
                .network_relay
                .send(NetworkMsg::Process(Command::PubSub(
                    PubSubCommand::Broadcast {
                        topic: self.settings.topic.clone(),
                        message: serialized.to_vec().into_boxed_slice(),
                    },
                )))
                .await
            {
                tracing::error!("failed to send item to topic: {e}");
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Settings<K, V> {
    pub topic: String,
    pub id: fn(&V) -> K,
}
