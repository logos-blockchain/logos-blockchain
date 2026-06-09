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

impl From<ServiceConfig> for NetworkConfig<Libp2pConfig> {
    fn from(value: ServiceConfig) -> Self {
        Self {
            backend: Libp2pConfig {
                initial_peers: value.user.backend.initial_peers,
                inner: SwarmConfig {
                    host: value.user.backend.swarm.host,
                    port: value.user.backend.swarm.port,
                    node_key: value.user.backend.swarm.node_key,
                    kad_protocol_name: value.deployment.kademlia_protocol_name,
                    identify_protocol_name: value.deployment.identify_protocol_name,
                    chain_sync_protocol_name: value.deployment.chain_sync_protocol_name,
                    gossipsub_config: value.user.backend.swarm.gossipsub.into(),
                    kademlia_config: KademliaSettings {
                        caching: value.user.backend.swarm.kademlia.caching.map(Into::into),
                        replication_factor: value.user.backend.swarm.kademlia.replication_factor,
                        parallelism: value.user.backend.swarm.kademlia.parallelism,
                        disjoint_query_paths: value
                            .user
                            .backend
                            .swarm
                            .kademlia
                            .disjoint_query_paths,
                        max_packet_size: value.user.backend.swarm.kademlia.max_packet_size,
                        kbucket_inserts: value
                            .user
                            .backend
                            .swarm
                            .kademlia
                            .kbucket_inserts
                            .map(Into::into),
                        periodic_bootstrap_interval_secs: value
                            .user
                            .backend
                            .swarm
                            .kademlia
                            .periodic_bootstrap_interval_secs,
                        query_timeout_secs: value.user.backend.swarm.kademlia.query_timeout_secs,
                    },
                    identify_config: IdentifySettings {
                        agent_version: value.user.backend.swarm.identify.agent_version,
                        cache_size: value.user.backend.swarm.identify.cache_size,
                        hide_listen_addrs: value.user.backend.swarm.identify.hide_listen_addrs,
                        interval_secs: value.user.backend.swarm.identify.interval_secs,
                        push_listen_addr_updates: value
                            .user
                            .backend
                            .swarm
                            .identify
                            .push_listen_addr_updates,
                    },
                    chain_sync_config: ChainSyncSettings {
                        peer_response_timeout: value
                            .user
                            .backend
                            .swarm
                            .chain_sync
                            .peer_response_timeout,
                        max_inbound_requests: value
                            .user
                            .backend
                            .swarm
                            .chain_sync
                            .max_inbound_requests,
                    },
                    nat_config: value.user.backend.swarm.nat.into(),
                },
            },
        }
    }
}
