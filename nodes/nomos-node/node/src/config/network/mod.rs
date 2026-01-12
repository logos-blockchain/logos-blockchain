use nomos_libp2p::SwarmConfig;
use nomos_network::{backends::libp2p::config::Libp2pConfig, config::NetworkConfig};

use crate::config::network::{deployment::Settings as DeploymentSettings, serde::Config};

pub mod deployment;
pub mod serde;

/// Libp2p network config which combines user-provided configuration with
/// deployment-specific settings.
///
/// Deployment-specific settings can refer to either a well-known deployment
/// (e.g., Nomos Mainnet), or to custom values.
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
                    chain_sync_config: value.user.backend.swarm.chain_sync_config,
                    gossipsub_config: value.user.backend.swarm.gossipsub_config,
                    host: value.user.backend.swarm.host,
                    identify_config: value.user.backend.swarm.identify_config,
                    identify_protocol_name: value.deployment.identify_protocol_name,
                    kad_protocol_name: value.deployment.kademlia_protocol_name,
                    chain_sync_protocol_name: value.deployment.chain_sync_protocol_name,
                    kademlia_config: value.user.backend.swarm.kademlia_config,
                    nat_config: value.user.backend.swarm.nat_config,
                    node_key: value.user.backend.swarm.node_key,
                    port: value.user.backend.swarm.port,
                },
            },
        }
    }
}
