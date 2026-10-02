use core::marker::PhantomData;
use std::sync::Arc;

use futures::Stream;
use lb_binary_codec::bincode::{self, DeserializeOp as _, SerializeOp as _};
use lb_core::block::MAX_BLOCK_TRANSACTIONS_SIZE;
use lb_cryptarchia_engine::{
    Slot,
    era::{Era, EraInForce, Eras},
};
use lb_era_parameters::EraDefinition;
use lb_log_targets::mempool;
use lb_network_service::{
    NetworkService,
    backends::libp2p::{Command, Libp2p, Message, PubSubCommand, TopicHash},
    message::NetworkMsg,
};
use lb_time_service::backends::TimeBackend;
use overwatch::services::{ServiceData, relay::OutboundRelay};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::watch;
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
    /// The topic of every era.
    topics: Arc<Eras<String>>,
    /// The eras in force the adapter follows, shared by its clones: `None`
    /// until it follows a slot.
    in_force: Arc<watch::Sender<Option<EraInForce>>>,
    _clock: PhantomData<fn() -> Clock>,
}

impl<Item, Key, Clock, RuntimeServiceId> Clone
    for Libp2pAdapter<Item, Key, Clock, RuntimeServiceId>
{
    fn clone(&self) -> Self {
        Self {
            network_relay: self.network_relay.clone(),
            settings: self.settings.clone(),
            topics: Arc::clone(&self.topics),
            in_force: Arc::clone(&self.in_force),
            _clock: PhantomData,
        }
    }
}

impl<Item, Key, Clock, RuntimeServiceId> Libp2pAdapter<Item, Key, Clock, RuntimeServiceId> {
    /// The topic the items of `era` are gossiped on.
    fn topic(&self, era: Era) -> &str {
        &self
            .topics
            .get(era)
            .expect("an era in force is scheduled")
            .entry
            .parameters
    }

    async fn send_pubsub_command(&self, command: PubSubCommand) {
        if let Err(error) = self
            .network_relay
            .send(NetworkMsg::Process(Command::PubSub(command)))
            .await
        {
            tracing::error!(target: LOG_TARGET, "error sending a pubsub command: {error}");
        }
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
        let topics = settings
            .eras
            .map(|era| era.entry.parameters.protocol_names.mempool_topic.clone());
        Self {
            network_relay,
            settings,
            topics: Arc::new(topics),
            in_force: Arc::new(watch::Sender::new(None)),
            _clock: PhantomData,
        }
    }

    async fn follow_eras_at(&self, slot: Slot) {
        let in_force = self.topics.in_force(slot);
        let previous = self.in_force.send_replace(Some(in_force));
        if previous == Some(in_force) {
            return;
        }
        let previously: Vec<Era> = previous.into_iter().flat_map(EraInForce::eras).collect();
        for era in in_force.eras().filter(|era| !previously.contains(era)) {
            let topic = self.topic(era).to_owned();
            tracing::debug!(
                target: LOG_TARGET,
                era = era.into_inner(),
                "Subscribing tx adapter to pubsub topic {topic}"
            );
            self.send_pubsub_command(PubSubCommand::Subscribe(topic))
                .await;
        }
        for era in previously
            .into_iter()
            .filter(|era| !in_force.eras().any(|in_force| in_force == *era))
        {
            let topic = self.topic(era).to_owned();
            tracing::debug!(
                target: LOG_TARGET,
                era = era.into_inner(),
                "Unsubscribing tx adapter from pubsub topic {topic}"
            );
            self.send_pubsub_command(PubSubCommand::Unsubscribe(topic))
                .await;
        }
    }

    async fn payload_stream(
        &self,
    ) -> Box<dyn Stream<Item = (Self::Key, Self::Payload)> + Unpin + Send> {
        let topics = self
            .topics
            .map(|era| TopicHash::from_raw(era.entry.parameters.clone()));
        let in_force = self.in_force.subscribe();
        let id = self.settings.id;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.network_relay
            .send(NetworkMsg::SubscribeToPubSub { sender })
            .await
            .expect("Network backend should be ready");

        let stream = receiver.await.unwrap();
        Box::new(Box::pin(stream.filter_map(move |message| match message {
            Ok(Message { data, topic, .. }) if is_in_force(&topics, *in_force.borrow(), &topic) => {
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

    async fn send(&self, item: Item) {
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
        // Broadcast on the topic of the era in force.
        let Some(in_force) = *self.in_force.borrow() else {
            tracing::error!(target: LOG_TARGET, "Not broadcasting before following the eras in force");
            return;
        };
        self.send_pubsub_command(PubSubCommand::Broadcast {
            topic: self.topic(in_force.era).to_owned(),
            message: serialized.to_vec().into_boxed_slice(),
        })
        .await;
    }
}

/// Whether `topic` is the topic of an era in force.
fn is_in_force(topics: &Eras<TopicHash>, in_force: Option<EraInForce>, topic: &TopicHash) -> bool {
    in_force
        .into_iter()
        .flat_map(EraInForce::eras)
        .filter_map(|era| topics.get(era))
        .any(|era| era.entry.parameters == *topic)
}

#[derive(Debug)]
pub struct Settings<K, V> {
    /// The chain's eras, whose transaction topics the adapter follows.
    pub eras: Arc<Eras<EraDefinition>>,
    pub id: fn(&V) -> K,
}

// Not derived, which would ask `K` and `V` to be `Clone`.
impl<K, V> Clone for Settings<K, V> {
    fn clone(&self) -> Self {
        Self {
            eras: Arc::clone(&self.eras),
            id: self.id,
        }
    }
}

#[cfg(test)]
mod tests {
    use core::{num::NonZero, time::Duration};

    use lb_core::mantle::{
        ledger::verification_mode::StandardMode,
        traits::StorageSize as _,
        transactions::{SignedOps, states::Preverified},
    };
    use lb_cryptarchia_engine::{
        Epoch,
        era::{EraEntry, EraVersion},
    };
    use time::OffsetDateTime;

    use super::*;

    fn topic(era: u32) -> TopicHash {
        TopicHash::from_raw(format!("/transactions/{era}"))
    }

    #[test]
    fn only_the_topics_of_the_eras_in_force_are_accepted() {
        // Era 1 starts at slot 10, and its first 5 slots still accept era 0.
        let topics = Eras::new(
            OffsetDateTime::UNIX_EPOCH,
            [0, 1].map(|era| EraEntry {
                first_epoch: Epoch::new(era),
                version: EraVersion::V1,
                slot_duration: Duration::from_secs(1),
                epoch_length: NonZero::new(10).unwrap(),
                transition_slots: 5,
                parameters: topic(era),
            }),
        )
        .unwrap();
        let accepted = |slot: u64| {
            let in_force = Some(topics.in_force(Slot::new(slot)));
            [0, 1].map(|era| is_in_force(&topics, in_force, &topic(era)))
        };

        assert_eq!(accepted(9), [true, false]);
        assert_eq!(accepted(10), [true, true]);
        assert_eq!(accepted(15), [false, true]);
        assert!(!is_in_force(&topics, None, &topic(0)));
    }

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
