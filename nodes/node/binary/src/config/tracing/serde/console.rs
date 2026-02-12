use core::net::{IpAddr, Ipv4Addr};

use lb_tracing_service::{ConsoleLayer, TokioConsoleConfig};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub enum Layer {
    Console(TokioConfig),
    #[default]
    None,
}

impl From<Layer> for ConsoleLayer {
    fn from(value: Layer) -> Self {
        match value {
            Layer::Console(config) => Self::Console(TokioConsoleConfig {
                bind_address: config.bind_address.to_string(),
                port: config.port,
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
}

impl Default for TokioConfig {
    fn default() -> Self {
        Self {
            bind_address: Ipv4Addr::UNSPECIFIED.into(),
            port: 9_000,
        }
    }
}
