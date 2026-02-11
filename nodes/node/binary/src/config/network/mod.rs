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
                inner: value.user.backend.swarm.into(),
            },
        }
    }
}
