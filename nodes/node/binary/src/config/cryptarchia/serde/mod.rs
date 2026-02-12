use serde::{Deserialize, Serialize};

pub mod leader;
pub mod network;
pub mod service;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub service: service::Config,
    #[serde(default)]
    pub network: network::Config,
    pub leader: leader::Config,
}
