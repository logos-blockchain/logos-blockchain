#![allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "Serde conditional serialization skip requires a specific function signature."
)]

use core::net::{IpAddr, Ipv4Addr};

use lb_tracing_service::{ConsoleLayer, TokioConsoleConfig};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TokioConfig {
    #[serde(default = "default_bind_address")]
    #[serde(skip_serializing_if = "is_default_bind_address")]
    pub bind_address: IpAddr,
    #[serde(default = "default_port")]
    #[serde(skip_serializing_if = "is_default_port")]
    pub port: u16,
}

fn default_bind_address() -> IpAddr {
    Ipv4Addr::UNSPECIFIED.into()
}

fn is_default_bind_address(addr: &IpAddr) -> bool {
    *addr == default_bind_address()
}

const fn default_port() -> u16 {
    9_000
}

const fn is_default_port(port: &u16) -> bool {
    *port == default_port()
}

impl Default for TokioConfig {
    fn default() -> Self {
        Self {
            bind_address: default_bind_address(),
            port: default_port(),
        }
    }
}
