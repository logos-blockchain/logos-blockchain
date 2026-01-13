use std::path::PathBuf;

use chain_leader::LeaderConfig;
use chain_network::SyncConfig;
use chain_service::StartingState;
use nomos_libp2p::PeerId;
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
    pub bootstrap: chain_service::BootstrapConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub bootstrap: chain_network::BootstrapConfig<PeerId>,
    pub sync: SyncConfig,
}
