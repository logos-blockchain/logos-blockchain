pub mod deployment;
mod dynamic;
pub(crate) mod node_configs;
pub mod postprocess;
pub mod wallet;
use lb_node::config::deployment::DeploymentSettings;

pub use crate::framework::local::build_node_run_config;

pub mod network {
    pub use super::node_configs::network::{
        Libp2pNetworkLayout as NetworkLayout, Libp2pNetworkLayout, NetworkParams,
    };
}

pub(crate) use dynamic::create_node_config_for_node;
use lb_config::deployment::e2e_deployment_settings_with_genesis_block;
pub use node_configs::GeneralConfig as Config;
pub(crate) use node_configs::{
    create_general_configs_from_ids as create_node_configs_from_ids,
    network::{Libp2pNetworkLayout, NetworkParams},
};

#[must_use]
pub fn default_e2e_deployment_settings(
    genesis_block: &lb_core::block::genesis::GenesisBlock,
) -> DeploymentSettings {
    e2e_deployment_settings_with_genesis_block(genesis_block)
}
