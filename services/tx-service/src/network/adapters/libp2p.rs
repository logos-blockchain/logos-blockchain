use std::{collections::HashMap, mem};

use futures::Stream;
use lb_binary_codec::canonical::{BinaryDecode, BinaryEncode};
use lb_log_targets::mempool;
use lb_network_service::{
    backends::libp2p::{Command, Libp2p, Message, PubSubCommand, TopicHash},
    message::{BackendNetworkMsg, NetworkMsg},
};
use lb_time::{Era, era::EraSchedule};
use overwatch::services::relay::OutboundRelay;
use tokio_stream::{StreamExt as _, wrappers::errors::BroadcastStreamRecvError};

use crate::network::NetworkAdapter;

const LOG_TARGET: &str = mempool::network::LIBP2P;

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
    Item: BinaryEncode + BinaryDecode<Context = u32> + Send + Sync + 'static + Clone,
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
        // the messages of every era's topic, each decoded under the limit of
        // that era, and follows the adapter from era to era, without requiring
        // any actions from consumers on era changes.
        let topics = self
            .settings
            .era_schedule
            .iter()
            .map(|era| era.parameters.topic.clone())
            .collect();
        let max_op_data_sizes: HashMap<TopicHash, u32> = self
            .settings
            .era_schedule
            .iter()
            .map(|era| {
                (
                    TopicHash::from_raw(era.parameters.topic.clone()),
                    era.parameters.max_op_data_size,
                )
            })
            .collect();
        let id = self.settings.id;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.network_relay
            .send(NetworkMsg::SubscribeToPubSub { topics, sender })
            .await
            .expect("Network backend should be ready");

        let stream = receiver.await.unwrap();
        Box::new(Box::pin(stream.filter_map(move |message| match message {
            Ok(Message { data, topic, .. }) => {
                match Item::decode_all(&data, max_op_data_sizes.get(&topic)?) {
                    Ok(item) => Some((id(&item), item)),
                    Err(e) => {
                        tracing::debug!(target: LOG_TARGET, "Unrecognized message: {e}");
                        None
                    }
                }
            }
            Err(BroadcastStreamRecvError::Lagged(n)) => {
                tracing::error!(target: LOG_TARGET, "lagged messages: {n}");
                None
            }
        })))
    }

    async fn send(&self, item: Item) {
        let size = item.encoded_length();
        let max_transaction_size = self
            .settings
            .era_schedule
            .get(self.current_era)
            .expect("The adapter is only ever in a scheduled era.")
            .parameters
            .max_transaction_size;
        if size > max_transaction_size {
            tracing::debug!(
                target: LOG_TARGET,
                size,
                maximum = max_transaction_size,
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
                        message: item.encode(),
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
    /// The most bytes a transaction may take: larger ones are not gossiped.
    pub max_transaction_size: usize,
    /// The most bytes of data an Operation may carry in the era: what the
    /// transactions of its topic are decoded under.
    pub max_op_data_size: u32,
}
