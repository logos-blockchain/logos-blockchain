use std::collections::HashMap;

use lb_core::{block::Proposal, mantle::transactions::genesis_tx::ChainId};
use lb_cryptarchia_engine::era::EraSchedule;
use lb_libp2p::{ChainSyncSettings, IdentifySettings, KademliaSettings, SwarmConfig};
use lb_network_service::{backends::libp2p::config::Libp2pConfig, config::NetworkConfig};
use lb_tx_service::network::adapters::libp2p::MAX_TRANSACTION_GOSSIP_BINCODE_PAYLOAD_SIZE;
use libp2p::gossipsub::{IdentTopic, TopicHash};

use crate::config::{
    deployment::{EraDefinition, ProtocolScope},
    network::serde::Config,
};

pub mod serde;

/// Libp2p network config: the user-provided configuration, completed with the
/// protocol names derived from the deployment.
pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    /// The settings of the network service in every era of `eras`, on the chain
    /// `chain_id`: each with its era's chain sync protocol and topics.
    #[must_use]
    pub fn into_network_service_era_schedule(
        self,
        chain_id: &ChainId,
        eras: &EraSchedule<EraDefinition>,
    ) -> EraSchedule<NetworkConfig<Libp2pConfig>> {
        let chain = ProtocolScope::Chain(chain_id);
        eras.map(|era| {
            let user = self.user.clone();
            let fork = ProtocolScope::Fork(era.entry.parameters.fork_digest);
            NetworkConfig {
                backend: Libp2pConfig {
                    initial_peers: user.backend.initial_peers,
                    max_data_size_by_topic: max_data_size_by_topic(
                        &fork.to_string_with_name("mempool"),
                        &fork.to_string_with_name("cryptarchia"),
                    ),
                    inner: SwarmConfig {
                        host: user.backend.swarm.host,
                        port: user.backend.swarm.port,
                        node_key: user.backend.swarm.node_key,
                        kad_protocol_name: chain.to_stream_protocol_with_name("kad"),
                        identify_protocol_name: chain.to_stream_protocol_with_name("identify"),
                        chain_sync_protocol_name: fork.to_stream_protocol_with_name("chainsync"),
                        gossipsub_config: user.backend.swarm.gossipsub.into(),
                        kademlia_config: KademliaSettings {
                            caching: user.backend.swarm.kademlia.caching.map(Into::into),
                            replication_factor: user.backend.swarm.kademlia.replication_factor,
                            parallelism: user.backend.swarm.kademlia.parallelism,
                            disjoint_query_paths: user.backend.swarm.kademlia.disjoint_query_paths,
                            max_packet_size: user.backend.swarm.kademlia.max_packet_size,
                            kbucket_inserts: user
                                .backend
                                .swarm
                                .kademlia
                                .kbucket_inserts
                                .map(Into::into),
                            periodic_bootstrap_interval_secs: user
                                .backend
                                .swarm
                                .kademlia
                                .periodic_bootstrap_interval_secs,
                            query_timeout_secs: user.backend.swarm.kademlia.query_timeout_secs,
                        },
                        identify_config: IdentifySettings {
                            agent_version: user.backend.swarm.identify.agent_version,
                            cache_size: user.backend.swarm.identify.cache_size,
                            hide_listen_addrs: user.backend.swarm.identify.hide_listen_addrs,
                            interval_secs: user.backend.swarm.identify.interval_secs,
                            push_listen_addr_updates: user
                                .backend
                                .swarm
                                .identify
                                .push_listen_addr_updates,
                        },
                        chain_sync_config: ChainSyncSettings {
                            peer_response_timeout: user
                                .backend
                                .swarm
                                .chain_sync
                                .peer_response_timeout,
                            max_inbound_requests: user
                                .backend
                                .swarm
                                .chain_sync
                                .max_inbound_requests,
                        },
                        nat_config: user.backend.swarm.nat.into(),
                    },
                },
            }
        })
    }
}

fn max_data_size_by_topic(
    transaction_topic: &str,
    proposal_topic: &str,
) -> HashMap<TopicHash, usize> {
    let mut limits: HashMap<TopicHash, usize> = HashMap::new();
    for (topic, required) in [
        (
            transaction_topic,
            MAX_TRANSACTION_GOSSIP_BINCODE_PAYLOAD_SIZE,
        ),
        (proposal_topic, Proposal::MAX_ENCODED_SIZE),
    ] {
        let topic = IdentTopic::new(topic).hash();
        limits
            .entry(topic)
            .and_modify(|existing| *existing = (*existing).max(required))
            .or_insert(required);
    }
    limits
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use lb_core::block::Proposal;
    use lb_libp2p::{
        PeerId,
        behaviour::gossipsub::configure_topic_size_limits,
        gossipsub::{self, RawMessage},
        identity::{self, ed25519},
    };

    use super::{MAX_TRANSACTION_GOSSIP_BINCODE_PAYLOAD_SIZE, max_data_size_by_topic};

    #[test]
    fn shared_application_topics_use_the_largest_data_limit() {
        let topic = "/shared/application/topic";
        let limits = max_data_size_by_topic(topic, topic);
        let topic_hash = gossipsub::IdentTopic::new(topic).hash();

        assert_eq!(
            limits.get(&topic_hash),
            Some(&MAX_TRANSACTION_GOSSIP_BINCODE_PAYLOAD_SIZE.max(Proposal::MAX_ENCODED_SIZE))
        );
    }

    #[test]
    fn production_payload_maxima_fit_the_author_envelopes() {
        let node_key = ed25519::SecretKey::generate();
        let keypair = identity::Keypair::from(ed25519::Keypair::from(node_key));
        let author = PeerId::from(keypair.public());
        let limits = [
            (
                "/logos-blockchain-standalone-local/mempool/1.0.0",
                MAX_TRANSACTION_GOSSIP_BINCODE_PAYLOAD_SIZE,
            ),
            (
                "/logos-blockchain-standalone-local/cryptarchia/1.0.0",
                Proposal::MAX_ENCODED_SIZE,
            ),
        ];

        let config = configure_topic_size_limits(
            gossipsub::Config::default(),
            author,
            limits
                .iter()
                .map(|(topic, max_data_size)| {
                    (gossipsub::IdentTopic::new(*topic).hash(), *max_data_size)
                })
                .collect::<HashMap<_, _>>(),
        )
        .unwrap();

        for (topic, max_payload_size) in limits {
            let topic_hash = gossipsub::IdentTopic::new(topic).hash();
            let raw_message = RawMessage {
                source: Some(author),
                data: vec![0; max_payload_size],
                sequence_number: Some(u64::MAX),
                topic: topic_hash.clone(),
                signature: None,
                key: None,
                validated: true,
            };

            assert_eq!(
                config.max_transmit_size_for_topic(&topic_hash),
                raw_message.raw_protobuf_len()
            );
            let config = gossipsub::ConfigBuilder::from(config.clone())
                .validation_mode(gossipsub::ValidationMode::None)
                .build()
                .unwrap();
            let mut behaviour = gossipsub::Behaviour::<
                gossipsub::IdentityTransform,
                gossipsub::AllowAllSubscriptionFilter,
            >::new(
                gossipsub::MessageAuthenticity::Author(author),
                config.clone(),
            )
            .unwrap();
            assert!(!matches!(
                behaviour.publish(gossipsub::IdentTopic::new(topic), vec![0; max_payload_size]),
                Err(gossipsub::PublishError::MessageTooLarge)
            ));
        }
    }
}
