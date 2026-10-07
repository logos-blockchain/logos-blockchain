//! Builds native Logos configuration from prepared network inputs.

pub mod deployment;
mod dynamic;
pub(crate) mod node_configs;
pub mod postprocess;
mod prepared_deployment;
mod render;

pub(crate) use dynamic::create_node_config_for_node;
use lb_config::deployment::e2e_deployment_settings_with_genesis_block;
use lb_node::config::deployment::DeploymentSettings;
pub use node_configs::GeneralConfig as Config;
pub(crate) use node_configs::network::{Libp2pNetworkLayout, NetworkParams};
pub use prepared_deployment::PreparedDeployment;
pub use render::build_plan;

pub use crate::{
    local::build_node_run_config,
    preparation::{SharedDeployment, wallet},
};

pub mod network {
    pub use super::node_configs::network::{
        Libp2pNetworkLayout as NetworkLayout, Libp2pNetworkLayout, NetworkParams,
    };
}

#[must_use]
pub fn default_e2e_deployment_settings(
    genesis_block: &lb_core::block::genesis::GenesisBlock,
) -> DeploymentSettings {
    e2e_deployment_settings_with_genesis_block(genesis_block)
}

#[must_use]
pub fn deployment_settings_for_topology(
    genesis_block: &lb_core::block::genesis::GenesisBlock,
    topology: &deployment::TopologyConfig,
) -> DeploymentSettings {
    let mut settings = default_e2e_deployment_settings(genesis_block);
    topology.apply_deployment_overrides(&mut settings);
    settings
}
