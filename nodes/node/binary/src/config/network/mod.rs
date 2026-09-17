use std::collections::HashMap;

use lb_libp2p::{ChainSyncSettings, IdentifySettings, KademliaSettings, SwarmConfig};
use lb_network_service::{backends::libp2p::config::Libp2pConfig, config::NetworkConfig};

use crate::config::network::{deployment::Settings as DeploymentSettings, serde::Config};

pub mod deployment;
pub mod serde;

/// Libp2p network config which combines user-provided configuration with
/// deployment-specific settings.
///
/// Deployment-specific settings can refer to either a well-known deployment
/// (e.g., Logos blockchain Mainnet), or to custom values.
pub struct ServiceConfig {
    pub user: Config,
    pub deployment: DeploymentSettings,
}

impl ServiceConfig {
    pub fn into_network_config(
        self,
        max_data_size_by_topic: HashMap<lb_libp2p::gossipsub::TopicHash, usize>,
    ) -> NetworkConfig<Libp2pConfig> {
        let Self { user, deployment } = self;

        NetworkConfig {
            backend: Libp2pConfig {
                initial_peers: user.backend.initial_peers,
                max_data_size_by_topic,
                inner: SwarmConfig {
                    host: user.backend.swarm.host,
                    port: user.backend.swarm.port,
                    node_key: user.backend.swarm.node_key,
                    kad_protocol_name: deployment.kademlia_protocol_name,
                    identify_protocol_name: deployment.identify_protocol_name,
                    chain_sync_protocol_name: deployment.chain_sync_protocol_name,
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
                        peer_response_timeout: user.backend.swarm.chain_sync.peer_response_timeout,
                        max_inbound_requests: user.backend.swarm.chain_sync.max_inbound_requests,
                    },
                    nat_config: user.backend.swarm.nat.into(),
                },
            },
        }
    }
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

    use crate::MAX_TRANSACTION_GOSSIP_BINCODE_PAYLOAD_SIZE;

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
