use lb_libp2p::{behaviour::gossipsub::swarm_ext::topic_hash, gossipsub};
use rand::RngCore;

use crate::backends::libp2p::{
    Command,
    swarm::{MAX_RETRY, SwarmHandler, exp_backoff},
};

pub type Topic = String;

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
    pub(super) fn handle_pubsub_command(&mut self, command: PubSubCommand) {
        match command {
            PubSubCommand::Broadcast { topic, message } => {
                self.broadcast_and_retry(topic, message, 0);
            }
            PubSubCommand::Subscribe(topic) => {
                tracing::trace!("subscribing to topic: {topic}");
                log_error!(self.swarm.subscribe(&topic));
            }
            PubSubCommand::Unsubscribe(topic) => {
                tracing::trace!("unsubscribing to topic: {topic}");
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
        tracing::trace!("broadcasting message to topic: {topic}");

        match self.swarm.broadcast(&topic, message.to_vec()) {
            Ok(id) => {
                tracing::trace!("Broadcasted message with id: {id} to topic: {topic}");
                // self-notification because libp2p doesn't do it.
                // Only do this on first attempt; if a previous attempt already
                // injected the message locally due to InsufficientPeers, avoid
                // notifying local subscribers twice.
                // TODO: Remove this logic once we start re-applying blocks produced locally. In
                // that case, we don't need to bubble this up since we have already applied the
                // block before broadcasting it.
                if retry_count == 0 && self.swarm.is_subscribed(&topic) {
                    log_error!(self.pubsub_messages_tx.send(gossipsub::Message {
                        source: None,
                        data: message.into(),
                        sequence_number: None,
                        topic: topic_hash(&topic),
                    }));
                }
            }
            Err(gossipsub::PublishError::InsufficientPeers) if retry_count < MAX_RETRY => {
                // TODO: Remove this logic once we start re-applying blocks produced locally. In
                // that case, we don't need to bubble this up since we have already applied the
                // block before broadcasting it. In single-node or transiently isolated setups,
                // publish can fail with InsufficientPeers. Still surface the
                // message locally once so higher layers can progress while
                // retries continue for eventual dissemination.
                if retry_count == 0 && self.swarm.is_subscribed(&topic) {
                    log_error!(self.pubsub_messages_tx.send(gossipsub::Message {
                        source: None,
                        data: message.clone().into(),
                        sequence_number: None,
                        topic: topic_hash(&topic),
                    }));
                }

                let wait = exp_backoff(retry_count);
                tracing::trace!(
                    "failed to broadcast message to topic due to insufficient peers, trying again in {wait:?}"
                );

                let commands_tx = self.commands_tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(wait).await;
                    let Some(new_retry_count) = retry_count.checked_add(1) else {
                        tracing::error!("retry count overflow.");
                        return;
                    };

                    commands_tx
                        .send(Command::PubSub(PubSubCommand::RetryBroadcast {
                            topic,
                            message,
                            retry_count: new_retry_count,
                        }))
                        .await
                        .unwrap_or_else(|_| tracing::error!("could not schedule retry"));
                });
            }
            Err(gossipsub::PublishError::Duplicate) => {
                tracing::trace!("not publishing duplicate message to topic: {topic}");
            }
            Err(e) => {
                tracing::error!("failed to broadcast message to topic: {topic} {e:?}");
            }
        }
    }

    pub(super) fn handle_gossipsub_event(&self, event: gossipsub::Event) {
        if let gossipsub::Event::Message { message, .. } = event
            && let Err(e) = self.pubsub_messages_tx.send(message)
        {
            tracing::error!("Failed to send gossipsub message event: {}", e);
        }
    }
}
