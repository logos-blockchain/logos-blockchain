use std::{collections::HashSet, mem};

use futures::Stream;
use lb_binary_codec::bincode::{self, DeserializeOp as _, SerializeOp as _};
use lb_core::block::MAX_BLOCK_TRANSACTIONS_SIZE;
use lb_log_targets::mempool;
use lb_network_service::{
    backends::libp2p::{Command, Libp2p, Message, PubSubCommand, TopicHash},
    message::{BackendNetworkMsg, NetworkMsg},
};
use lb_time::{Era, era::EraSchedule};
use overwatch::services::relay::OutboundRelay;
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

#[derive(Clone)]
pub struct Libp2pAdapter<Item, Key, RuntimeServiceId> {
    network_relay: OutboundRelay<BackendNetworkMsg<Libp2p, RuntimeServiceId>>,
    settings: Settings<Key, Item>,
    /// The era the adapter is in.
    current_era: Era,
    /// The topic of the era the adapter is in.
    current_era_topic: String,
    /// The topic of the era before, which the adapter stays subscribed to
    /// until the end of its transition period.
    previous_era_topic: Option<String>,
}

impl<Item, Key, RuntimeServiceId> Libp2pAdapter<Item, Key, RuntimeServiceId> {
    /// Stops receiving transactions on `topic`.
    async fn unsubscribe(
        network_relay: &OutboundRelay<BackendNetworkMsg<Libp2p, RuntimeServiceId>>,
        topic: String,
    ) {
        if let Err(error) = network_relay
            .send(NetworkMsg::Process(Command::PubSub(
                PubSubCommand::Unsubscribe(topic),
            )))
            .await
        {
            tracing::error!(target: LOG_TARGET, "failed to unsubscribe from topic: {error}");
        }
    }
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
        current_era: Era,
        network_relay: OutboundRelay<BackendNetworkMsg<Self::Backend, RuntimeServiceId>>,
    ) -> Self {
        let current_era_topic = settings
            .era_schedule
            .get(current_era)
            .expect("The era the adapter starts in should be scheduled")
            .parameters
            .topic
            .clone();
        tracing::debug!(
            target: LOG_TARGET,
            "Subscribing tx adapter to pubsub topic {current_era_topic}"
        );
        network_relay
            .send(NetworkMsg::Process(Command::PubSub(
                PubSubCommand::Subscribe(current_era_topic.clone()),
            )))
            .await
            .expect("Network backend should be ready");
        Self {
            network_relay,
            settings,
            current_era,
            current_era_topic,
            previous_era_topic: None,
        }
    }

    async fn rotate_era(&mut self, era: Era) {
        let Some(era_settings) = self.settings.era_schedule.get(era) else {
            tracing::error!(
                target: LOG_TARGET,
                "Asked to enter era {}, which is not scheduled",
                era.into_inner()
            );
            return;
        };
        self.current_era = era;
        let new_era_topic = era_settings.parameters.topic.clone();
        if new_era_topic == self.current_era_topic {
            return;
        }
        tracing::info!(
            target: LOG_TARGET,
            "Entered era {}: transactions are gossiped on {new_era_topic}",
            era.into_inner()
        );
        // A topic of an earlier era still held has had its transition period
        // end already: only the report of its end went missing.
        if let Some(lingering_topic) = self.previous_era_topic.take() {
            tracing::warn!(
                target: LOG_TARGET,
                "The end of the previous era's transition period was never reported: transactions are no longer received on {lingering_topic}"
            );
            Self::unsubscribe(&self.network_relay, lingering_topic).await;
        }
        let previous_era_topic = mem::replace(&mut self.current_era_topic, new_era_topic.clone());
        if let Err(error) = self
            .network_relay
            .send(NetworkMsg::Process(Command::PubSub(
                PubSubCommand::Subscribe(new_era_topic),
            )))
            .await
        {
            tracing::error!(target: LOG_TARGET, "failed to subscribe to topic: {error}");
        }
        self.previous_era_topic = Some(previous_era_topic);
    }

    async fn complete_era_transition(&mut self, ended_era: Era) {
        if self.current_era.checked_sub(1) != Some(ended_era) {
            return;
        }
        let Some(previous_era_topic) = self.previous_era_topic.take() else {
            return;
        };
        tracing::info!(
            target: LOG_TARGET,
            "Transition period over: transactions are no longer received on {previous_era_topic}"
        );
        Self::unsubscribe(&self.network_relay, previous_era_topic).await;
    }

    async fn payload_stream(
        &self,
    ) -> Box<dyn Stream<Item = (Self::Key, Self::Payload)> + Unpin + Send> {
        // Gossipsub only hands over the messages of the topics the node is
        // subscribed to: the topic of the adapter's current era, and the topic
        // of the era before during its transition period. So the stream keeps
        // the messages of every era's topic, and follows the adapter from era
        // to era, without requiring any actions from consumers on era changes.
        let topic_hashes: HashSet<TopicHash> = self
            .settings
            .era_schedule
            .iter()
            .map(|era| TopicHash::from_raw(era.parameters.topic.clone()))
            .collect();
        let id = self.settings.id;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.network_relay
            .send(NetworkMsg::SubscribeToPubSub { sender })
            .await
            .expect("Network backend should be ready");

        let stream = receiver.await.unwrap();
        Box::new(Box::pin(stream.filter_map(move |message| match message {
            Ok(Message { data, topic, .. }) if topic_hashes.contains(&topic) => {
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
        {
            if let Err(error) = self
                .network_relay
                .send(NetworkMsg::Process(Command::PubSub(
                    PubSubCommand::Broadcast {
                        topic: self.current_era_topic.clone(),
                        message: serialized.to_vec().into_boxed_slice(),
                    },
                )))
                .await
            {
                tracing::error!(target: LOG_TARGET, "failed to send item to topic: {error}");
            }
        }
    }
}

/// The settings of the libp2p adapter: those it keeps for the whole run, and
/// those that change from era to era.
#[derive(Clone, Debug)]
pub struct Settings<K, V> {
    pub id: fn(&V) -> K,
    pub era_schedule: EraSchedule<EraSettings>,
}

/// What the libp2p adapter runs with in an era.
#[derive(Clone, Debug)]
pub struct EraSettings {
    /// The topic transactions are gossiped on.
    pub topic: String,
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
