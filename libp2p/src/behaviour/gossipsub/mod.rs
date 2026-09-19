use std::{collections::HashMap, hash::BuildHasher};

use blake2::{Blake2b, Digest as _, digest::consts::U32};
use libp2p::{PeerId, gossipsub};

pub mod swarm_ext;

/// Adds application payload limits to a Gossipsub config.
///
/// Gossipsub applies its per-topic limit differently on the send and receive
/// paths: outbound publish compares transformed data directly with the
/// configured limit, while inbound decoding compares the encoded protobuf
/// Message size. The current behaviour publishes with
/// `MessageAuthenticity::Author`, so the configured limit is derived from the
/// actual unsigned `RawMessage` representation containing the author, sequence
/// number, data, and raw topic. The supplied maximum is the size of already
/// serialized application data; this layer does not know which application
/// serializer produced it.
/// The node currently derives that author from its configured Ed25519 identity
/// and publishes unsigned messages without signature or key fields. Changes to
/// the authentication mode, signing, identity representation, public-key
/// inclusion, or data transform require revisiting this envelope calculation.
pub fn configure_topic_size_limits<S>(
    config: gossipsub::Config,
    author: PeerId,
    max_data_size_by_topic: HashMap<gossipsub::TopicHash, usize, S>,
) -> Result<gossipsub::Config, gossipsub::ConfigBuilderError>
where
    S: BuildHasher,
{
    let mut builder = gossipsub::ConfigBuilder::from(config);
    for (topic, max_data_size) in max_data_size_by_topic {
        let transmit_size = gossipsub_message_size(&topic, &author, max_data_size);
        builder.max_transmit_size_for_topic(transmit_size, topic);
    }

    builder.build()
}

fn gossipsub_message_size(
    topic: &gossipsub::TopicHash,
    author: &PeerId,
    max_data_size: usize,
) -> usize {
    gossipsub::RawMessage {
        source: Some(*author),
        data: vec![0; max_data_size],
        sequence_number: Some(u64::MAX),
        topic: topic.clone(),
        signature: None,
        key: None,
        validated: true,
    }
    .raw_protobuf_len()
}

#[must_use]
pub fn compute_message_id(message: &gossipsub::Message) -> gossipsub::MessageId {
    let mut hasher = Blake2b::<U32>::new();
    hasher.update(&message.data);
    gossipsub::MessageId::from(hasher.finalize().to_vec())
}

#[cfg(test)]
mod tests {
    use lb_utils::net::MAX_WIRE_MESSAGE_SIZE;
    use libp2p::gossipsub::{ConfigBuilder, MessageAuthenticity, PublishError, ValidationMode};

    use super::*;

    fn author() -> PeerId {
        PeerId::random()
    }

    #[test]
    fn topic_limit_is_derived_from_serialized_application_data_size() {
        let author = PeerId::random();
        let topic = gossipsub::IdentTopic::new("shared").hash();
        let max_data_size = 1024;
        let config = configure_topic_size_limits(
            gossipsub::Config::default(),
            author,
            HashMap::from([(topic.clone(), max_data_size)]),
        )
        .unwrap();

        assert_eq!(
            config.max_transmit_size_for_topic(&topic),
            gossipsub_message_size(&topic, &author, max_data_size)
        );
    }

    #[test]
    fn topic_limit_is_not_restricted_by_the_default_maximum() {
        let author = PeerId::random();
        let topic = gossipsub::IdentTopic::new("shared").hash();
        let max_data_size = MAX_WIRE_MESSAGE_SIZE;
        let config = configure_topic_size_limits(
            gossipsub::Config::default(),
            author,
            HashMap::from([(topic.clone(), max_data_size)]),
        )
        .unwrap();

        assert!(config.max_transmit_size_for_topic(&topic) > max_data_size);
    }

    #[test]
    fn gossipsub_rejects_payloads_over_the_topic_limit() {
        let author = author();
        let topic = "transactions";
        let topic_hash = gossipsub::IdentTopic::new(topic).hash();
        let config = configure_topic_size_limits(
            gossipsub::Config::default(),
            author,
            HashMap::from([(topic_hash.clone(), 512)]),
        )
        .unwrap();
        let topic_limit = config.max_transmit_size_for_topic(&topic_hash);
        let config = ConfigBuilder::from(config)
            .validation_mode(ValidationMode::None)
            .build()
            .unwrap();
        let mut behaviour = gossipsub::Behaviour::<
            gossipsub::IdentityTransform,
            gossipsub::AllowAllSubscriptionFilter,
        >::new(MessageAuthenticity::Author(author), config)
        .unwrap();

        assert!(matches!(
            behaviour.publish(gossipsub::IdentTopic::new(topic), vec![0; topic_limit + 1]),
            Err(PublishError::MessageTooLarge)
        ));
    }
}
