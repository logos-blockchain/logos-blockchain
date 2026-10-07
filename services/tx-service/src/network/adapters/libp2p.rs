use core::{marker::PhantomData, mem};

use futures::{Stream, future::BoxFuture};
use lb_binary_codec::bincode::{self, DeserializeOp as _, SerializeOp as _};
use lb_core::block::MAX_BLOCK_TRANSACTIONS_SIZE;
use lb_log_targets::mempool;
use lb_network_service::{
    NetworkService,
    backends::libp2p::{Command, Libp2p, Message, PubSubCommand, TopicHash},
    message::NetworkMsg,
};
use lb_time_service::backends::TimeBackend;
use lb_utils::tokio::task::spawn;
use overwatch::services::{ServiceData, relay::OutboundRelay};
use serde::{Serialize, de::DeserializeOwned};
use tokio_stream::StreamExt as _;

use crate::network::NetworkAdapter;

const LOG_TARGET: &str = mempool::network::LIBP2P;

/// Direct transaction gossip carries canonical bytes in a configured-bincode
/// byte envelope.
///
/// The mempool's existing transaction-content limit therefore leaves one
/// bincode `u64` length prefix of overhead.
/// This is the application payload limit, not the Gossipsub protobuf limit.
pub const MAX_TRANSACTION_GOSSIP_BINCODE_PAYLOAD_SIZE: usize =
    MAX_BLOCK_TRANSACTIONS_SIZE + bincode::BINCODE_LENGTH_PREFIX_SIZE;

#[must_use]
const fn transaction_gossip_size_is_valid(size: usize) -> bool {
    size <= MAX_TRANSACTION_GOSSIP_BINCODE_PAYLOAD_SIZE
}

pub struct Libp2pAdapter<Item, Key, Clock, RuntimeServiceId> {
    network_relay:
        OutboundRelay<<NetworkService<Libp2p, RuntimeServiceId> as ServiceData>::Message>,
    settings: Settings<Key, Item>,
    _clock: PhantomData<fn() -> Clock>,
}

/// Leaves the era's topic.
impl<Item, Key, Clock, RuntimeServiceId> Drop
    for Libp2pAdapter<Item, Key, Clock, RuntimeServiceId>
{
    fn drop(&mut self) {
        // Dropping cannot wait for the network service: a task of its own sends
        // the unsubscribe.
        let topic = mem::take(&mut self.settings.topic);
        let network_relay = self.network_relay.clone();
        drop(spawn("logos/mempool/unsubscribe", async move {
            tracing::debug!(target: LOG_TARGET, "Unsubscribing tx adapter from pubsub topic {topic}");
            if let Err(error) = network_relay
                .send(NetworkMsg::Process(Command::PubSub(
                    PubSubCommand::Unsubscribe(topic),
                )))
                .await
            {
                tracing::error!(target: LOG_TARGET, "failed to leave the topic: {error}");
            }
        }));
    }
}

#[async_trait::async_trait]
impl<Item, Key, Clock, RuntimeServiceId> NetworkAdapter<RuntimeServiceId>
    for Libp2pAdapter<Item, Key, Clock, RuntimeServiceId>
where
    Item: DeserializeOwned + Serialize + Send + Sync + 'static + Clone,
    Key: Clone + Send + Sync + 'static,
    Clock: TimeBackend + 'static,
{
    type Backend = Libp2p;
    type Settings = Settings<Key, Item>;
    type Payload = Item;
    type Key = Key;
    type TimeBackend = Clock;

    async fn new(
        settings: Self::Settings,
        network_relay: OutboundRelay<
            <NetworkService<Self::Backend, RuntimeServiceId> as ServiceData>::Message,
        >,
    ) -> Self {
        tracing::debug!(
            target: LOG_TARGET,
            "Subscribing tx adapter to pubsub topic {}",
            settings.topic
        );
        network_relay
            .send(NetworkMsg::Process(Command::PubSub(
                PubSubCommand::Subscribe(settings.topic.clone()),
            )))
            .await
            .expect("Network backend should be ready");
        Self {
            network_relay,
            settings,
            _clock: PhantomData,
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
                        tracing::debug!(target: LOG_TARGET, "Unrecognized message: {e}");
                        None
                    }
                }
            }
            _ => None,
        })))
    }

    fn send(&self, item: Item) -> BoxFuture<'static, ()> {
        let network_relay = self.network_relay.clone();
        let topic = self.settings.topic.clone();
        Box::pin(async move {
            let serialized = item
                .to_bytes()
                .expect("Item should be able to be serialized");
            if !transaction_gossip_size_is_valid(serialized.len()) {
                tracing::debug!(
                    target: LOG_TARGET,
                    size = serialized.len(),
                    maximum = MAX_TRANSACTION_GOSSIP_BINCODE_PAYLOAD_SIZE,
                    "Not broadcasting an oversized transaction"
                );
                return;
            }
            if let Err(error) = network_relay
                .send(NetworkMsg::Process(Command::PubSub(
                    PubSubCommand::Broadcast {
                        topic,
                        message: serialized.to_vec().into_boxed_slice(),
                    },
                )))
                .await
            {
                tracing::error!(target: LOG_TARGET, "failed to send item to topic: {error}");
            }
        })
    }
}

#[derive(Clone, Debug)]
pub struct Settings<K, V> {
    pub topic: String,
    pub id: fn(&V) -> K,
}

#[cfg(test)]
mod tests {
    use lb_core::mantle::{
        ledger::verification_mode::StandardMode,
        traits::StorageSize as _,
        transactions::{SignedOps, states::Preverified},
    };

    use super::*;

    #[test]
    fn transaction_gossipsub_bound_accounts_for_the_bincode_envelope() {
        let transaction = SignedOps::<Preverified, StandardMode>::empty();
        let bytes =
            <SignedOps<Preverified, StandardMode> as bincode::SerializeOp>::to_bytes(&transaction)
                .unwrap();

        assert_eq!(
            bytes.len(),
            transaction.storage_size() + bincode::BINCODE_LENGTH_PREFIX_SIZE
        );
        assert_eq!(
            MAX_TRANSACTION_GOSSIP_BINCODE_PAYLOAD_SIZE,
            MAX_BLOCK_TRANSACTIONS_SIZE + bincode::BINCODE_LENGTH_PREFIX_SIZE
        );
    }

    #[test]
    fn transaction_gossip_guard_rejects_one_byte_over_the_bound() {
        assert!(transaction_gossip_size_is_valid(
            MAX_TRANSACTION_GOSSIP_BINCODE_PAYLOAD_SIZE
        ));
        assert!(!transaction_gossip_size_is_valid(
            MAX_TRANSACTION_GOSSIP_BINCODE_PAYLOAD_SIZE + 1
        ));
    }
}
