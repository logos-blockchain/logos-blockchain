use std::path::PathBuf;

use logos_blockchain_chain_leader_service::LeaderConfig;
use logos_blockchain_chain_network_service::SyncConfig;
use logos_blockchain_chain_service::StartingState;
use logos_blockchain_libp2p::PeerId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub service: ServiceConfig,
    pub network: NetworkConfig,
    pub leader: LeaderConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub starting_state: StartingState,
    pub recovery_file: PathBuf,
    pub bootstrap: logos_blockchain_chain_service::BootstrapConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub bootstrap: logos_blockchain_chain_network_service::BootstrapConfig<PeerId>,
    pub sync: SyncConfig,
}
