use std::collections::HashMap;

use lb_libp2p::{behaviour::gossipsub::swarm_ext::topic_hash, gossipsub};
use lb_log_targets::network_service;
use lb_utils::tokio::task::spawn;
use rand::RngCore;

use crate::backends::libp2p::{
    Command,
    swarm::{MAX_RETRY, SwarmHandler, exp_backoff},
};

pub type Topic = String;

const LOG_TARGET: &str = network_service::backends::libp2p::GOSSIPSUB;

fn application_data_size_is_valid(
    max_data_size_by_topic: &HashMap<gossipsub::TopicHash, usize>,
    topic: &gossipsub::TopicHash,
    data_size: usize,
) -> bool {
    max_data_size_by_topic
        .get(topic)
        .is_some_and(|max_data_size| data_size <= *max_data_size)
}

fn is_within_application_data_limit(
    max_data_size_by_topic: &HashMap<gossipsub::TopicHash, usize>,
    message: &gossipsub::Message,
) -> bool {
    application_data_size_is_valid(max_data_size_by_topic, &message.topic, message.data.len())
}

#[derive(Debug)]
#[non_exhaustive]
pub enum PubSubCommand {
    Broadcast {
        topic: Topic,
        message: Box<[u8]>,
    },
    Subscribe(Topic),
    Unsubscribe(Topic),
    #[doc(hidden)]
    RetryBroadcast {
        topic: Topic,
        message: Box<[u8]>,
        retry_count: usize,
    },
}

impl<R: Clone + Send + RngCore + 'static> SwarmHandler<R> {
    #[expect(
        clippy::cognitive_complexity,
        reason = "This command dispatcher intentionally handles all PubSub commands."
    )]
    pub(super) fn handle_pubsub_command(&mut self, command: PubSubCommand) {
        match command {
            PubSubCommand::Broadcast { topic, message } => {
                self.broadcast_and_retry(topic, message, 0);
            }
            PubSubCommand::Subscribe(topic) => {
                if !self
                    .max_data_size_by_topic
                    .contains_key(&topic_hash(&topic))
                {
                    tracing::warn!(
                        target: LOG_TARGET,
                        "refusing to subscribe to gossipsub topic without an application data limit: {topic}"
                    );
                    return;
                }
                tracing::trace!(target: LOG_TARGET, "subscribing to topic: {topic}");
                log_error!(self.swarm.subscribe(&topic));
            }
            PubSubCommand::Unsubscribe(topic) => {
                tracing::trace!(target: LOG_TARGET, "unsubscribing to topic: {topic}");
                self.swarm.unsubscribe(&topic);
            }
            PubSubCommand::RetryBroadcast {
                topic,
                message,
                retry_count,
            } => {
                self.broadcast_and_retry(topic, message, retry_count);
            }
        }
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: address this in a dedicated refactor"
    )]
    pub(super) fn broadcast_and_retry(
        &mut self,
        topic: Topic,
        message: Box<[u8]>,
        retry_count: usize,
    ) {
        tracing::trace!(target: LOG_TARGET, "broadcasting message to topic: {topic}");

        let topic_hash_value = topic_hash(&topic);
        let Some(max_data_size) = self.max_data_size_by_topic.get(&topic_hash_value).copied()
        else {
            tracing::warn!(
                target: LOG_TARGET,
                "refusing to broadcast to gossipsub topic without an application data limit: {topic}"
            );
            return;
        };
        if !application_data_size_is_valid(
            &self.max_data_size_by_topic,
            &topic_hash_value,
            message.len(),
        ) {
            tracing::warn!(
                target: LOG_TARGET,
                topic = %topic,
                message_size = message.len(),
                max_data_size,
                "refusing to broadcast oversized gossipsub application data"
            );
            return;
        }

        match self.swarm.broadcast(&topic, message.to_vec()) {
            Ok(id) => {
                tracing::trace!(
                    target: LOG_TARGET,
                    "Broadcasted message with id: {id} to topic: {topic}"
                );
                // self-notification because libp2p doesn't do it
                if self.swarm.is_subscribed(&topic) {
                    log_error!(self.pubsub_messages_tx.send(gossipsub::Message {
                        source: None,
                        data: message.into(),
                        sequence_number: None,
                        topic: topic_hash(&topic),
                    }));
                }
            }
            Err(gossipsub::PublishError::NoPeersSubscribedToTopic) if retry_count < MAX_RETRY => {
                let wait = exp_backoff(retry_count);
                tracing::trace!(
                    target: LOG_TARGET,
                    "failed to broadcast message to topic due to insufficient peers, trying again in {wait:?}"
                );

                let commands_tx = self.commands_tx.clone();
                spawn("logos/network/gossipsub-retry", async move {
                    tokio::time::sleep(wait).await;
                    let Some(new_retry_count) = retry_count.checked_add(1) else {
                        tracing::error!(target: LOG_TARGET, "retry count overflow.");
                        return;
                    };

                    commands_tx
                        .send(Command::PubSub(PubSubCommand::RetryBroadcast {
                            topic,
                            message,
                            retry_count: new_retry_count,
                        }))
                        .await
                        .unwrap_or_else(|_| {
                            tracing::error!(target: LOG_TARGET, "could not schedule retry");
                        });
                });
            }
            Err(gossipsub::PublishError::Duplicate) => {
                tracing::trace!(
                    target: LOG_TARGET,
                    "not publishing duplicate message to topic: {topic}"
                );
            }
            Err(e) => {
                tracing::error!(
                    target: LOG_TARGET,
                    "failed to broadcast message to topic: {topic} {e:?}"
                );
            }
        }
    }

    pub(super) fn handle_gossipsub_event(&self, event: gossipsub::Event) {
        if let gossipsub::Event::Message { message, .. } = event {
            let Some(max_data_size) = self.max_data_size_by_topic.get(&message.topic).copied()
            else {
                tracing::warn!(
                    target: LOG_TARGET,
                    topic = ?message.topic,
                    "dropping gossipsub application data for a topic without a configured data-size limit"
                );
                return;
            };

            if !is_within_application_data_limit(&self.max_data_size_by_topic, &message) {
                tracing::warn!(
                    target: LOG_TARGET,
                    topic = ?message.topic,
                    message_size = message.data.len(),
                    max_data_size,
                    "Dropping oversized inbound gossipsub application data"
                );
                return;
            }

            if let Err(e) = self.pubsub_messages_tx.send(message) {
                tracing::error!(target: LOG_TARGET, "Failed to send gossipsub message event: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_data_limit_is_exact_for_outbound_sizes() {
        let topic = gossipsub::IdentTopic::new("transactions").hash();
        let max_data_size = 512;
        let limits = HashMap::from([(topic.clone(), max_data_size)]);

        assert!(application_data_size_is_valid(
            &limits,
            &topic,
            max_data_size
        ));
        assert!(!application_data_size_is_valid(
            &limits,
            &topic,
            max_data_size + 1
        ));
        assert!(!application_data_size_is_valid(
            &limits,
            &gossipsub::IdentTopic::new("unconfigured").hash(),
            max_data_size
        ));
    }
}
