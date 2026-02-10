use std::path::PathBuf;

use lb_chain_leader_service::LeaderWalletConfig;
use lb_chain_network_service::SyncConfig;
use lb_libp2p::PeerId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub service: ServiceConfig,
    pub network: NetworkConfig,
    pub leader: LeaderConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub recovery_file: PathBuf,
    pub bootstrap: lb_chain_service::BootstrapConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub bootstrap: lb_chain_network_service::BootstrapConfig<PeerId>,
    pub sync: SyncConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LeaderConfig {
    pub wallet: LeaderWalletConfig,
}
