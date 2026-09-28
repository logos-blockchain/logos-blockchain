use serde::{Deserialize, Serialize};

pub mod leader;
pub mod network;
pub mod service;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub service: service::Config,
    #[serde(default)]
    pub network: network::Config,
    #[serde(default)]
    pub leader: leader::Config,
}
