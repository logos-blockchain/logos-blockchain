use core::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use lb_tracing_service::{ConsoleLayerSettings, TokioConsoleConfig};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub enum Layer {
    Console(TokioConfig),
    #[default]
    None,
}

impl From<Layer> for ConsoleLayerSettings {
    fn from(value: Layer) -> Self {
        match value {
            Layer::Console(config) => Self::Console(TokioConsoleConfig {
                bind_address: config.bind_address.to_string(),
                port: config.port,
                recording_path: config.recording_path,
            }),
            Layer::None => Self::None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct TokioConfig {
    pub bind_address: IpAddr,
    pub port: u16,
    pub recording_path: Option<PathBuf>,
}

impl Default for TokioConfig {
    fn default() -> Self {
        Self {
            bind_address: Ipv4Addr::LOCALHOST.into(),
            port: 6_669,
            recording_path: None,
        }
    }
}
