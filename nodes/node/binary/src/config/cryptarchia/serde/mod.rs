use serde::{Deserialize, Serialize};

pub mod leader;
pub mod network;
pub mod service;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub service: service::Config,
    pub network: network::Config,
    pub leader: leader::Config,
}
